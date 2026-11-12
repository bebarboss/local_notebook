//! `.xlsx` `.xlsm` `.xls` `.ods` `.csv` `.tsv` — every row becomes "header: value" lines.
//!
//! A row is one record (an incident, an employee, an order…). It is written as
//!
//! ```text
//! ชีต Main · แถว 3 · 2026/045234
//! Incident No: 2026/045234
//! Status: Finish
//! รายละเอียดปัญหา: …
//! ```
//!
//! Empty cells and filler ("-", "00.00") are left out, long shared header prefixes
//! ("Customer and Information Detail …") are dropped, and a long row is cut into pieces
//! that each start with the same title line, so every chunk says which record it is.
//! `Page::page` holds the row number (the UI shows "แถว", not "น.").
//!
//! Sheets made for people rather than databases are handled too: merged cells, a two-row
//! header (a merged group over sub-headings), several tables stacked with blank rows
//! between them, a group value written once over a run of rows, and field templates
//! laid out sideways (fields across, "description / example / required" down), which
//! become one card per field.

use super::chunker::{chunk, ChunkOptions};
use super::{Extractor, Page};
use crate::errors::{AppError, AppResult};
use calamine::{open_workbook_auto, Data, Reader, Sheets};
use std::collections::HashMap;
use std::path::Path;

pub const EXTENSIONS: [&str; 6] = ["xlsx", "xlsm", "xls", "ods", "csv", "tsv"];

/// Characters per piece of a row, kept under the default chunk size so the chunker leaves
/// the pieces alone.
const PIECE: usize = 900;
/// Characters per piece when short rows are grouped (smaller, so a search still lands near
/// the right row).
const GROUP: usize = 500;

pub struct SheetExtractor;

impl Extractor for SheetExtractor {
    fn extensions(&self) -> &'static [&'static str] {
        &EXTENSIONS
    }

    fn extract(&self, path: &Path) -> AppResult<Vec<Page>> {
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let sheets = if ext == "csv" || ext == "tsv" {
            vec![Sheet::plain(read_delimited(&std::fs::read(path)?))]
        } else {
            match read_workbook(path) {
                Ok(s) => s,
                // reports saved as ".xls" are often tab-separated text
                Err(e) => {
                    let bytes = std::fs::read(path)?;
                    let text = decode(&bytes);
                    if text.contains('\t') && !text.trim_start().starts_with('<') {
                        vec![Sheet::plain(read_delimited(&bytes))]
                    } else {
                        return Err(e);
                    }
                }
            }
        };
        let many = sheets.iter().filter(|s| s.rows.len() > 1).count() > 1;
        let mut pages = Vec::new();
        for sh in sheets {
            let label = if many || !sh.name.is_empty() && sh.name != "Sheet1" { format!("ชีต {} · ", sh.name) } else { String::new() };
            pages.extend(rows_to_pages(&label, &sh.rows, &sh.merges, sh.row0));
        }
        if pages.is_empty() {
            return Err(AppError::new("no_text", "ข้าม · ไม่พบข้อมูลในตาราง"));
        }
        Ok(pages)
    }
}

/// A merged area, as (row, col) of its first and last cell, relative to `Sheet::rows`.
type Merge = ((usize, usize), (usize, usize));

struct Sheet {
    name: String,
    rows: Vec<Vec<String>>,
    merges: Vec<Merge>,
    /// Excel row of `rows[0]`, minus one
    row0: usize,
}

impl Sheet {
    fn plain(rows: Vec<Vec<String>>) -> Sheet {
        Sheet { name: String::new(), rows, merges: Vec::new(), row0: 0 }
    }
}

fn read_workbook(path: &Path) -> AppResult<Vec<Sheet>> {
    let mut wb = open_workbook_auto(path).map_err(|e| AppError::new("sheet", format!("อ่านไฟล์ Excel ไม่ได้: {e}")))?;
    let mut out = Vec::new();
    for name in wb.sheet_names().to_vec() {
        let Ok(range) = wb.worksheet_range(&name) else { continue };
        let (r0, c0) = range.start().map(|(r, c)| (r as usize, c as usize)).unwrap_or((0, 0));
        let rows: Vec<Vec<String>> = range.rows().map(|r| r.iter().map(cell_text).collect()).collect();
        let areas = match &mut wb {
            Sheets::Xlsx(x) => x.merge_cells_by_sheet_name(&name).unwrap_or_default(),
            Sheets::Xls(x) => x.merge_cells_by_sheet_name(&name).unwrap_or_default(),
            _ => Vec::new(),
        };
        let merges = areas
            .iter()
            .filter(|d| d.start.0 as usize >= r0 && d.start.1 as usize >= c0)
            .map(|d| {
                let at = |(r, c): (u32, u32)| (r as usize - r0, c as usize - c0);
                (at(d.start), at((d.end.0.max(d.start.0), d.end.1.max(d.start.1))))
            })
            .collect();
        out.push(Sheet { name, rows, merges, row0: r0 });
    }
    Ok(out)
}

fn cell_text(c: &Data) -> String {
    match c {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        // 5727261.0 → "5727261"
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Data::Float(f) => f.to_string(),
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(d) => excel_date(d.as_f64()),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(_) => String::new(),
    }
}

/// Excel serial date → "dd/mm/yyyy" or "dd/mm/yyyy hh:mm:ss".
fn excel_date(serial: f64) -> String {
    let days = serial.floor() as i64;
    // 1899-12-30 is day 0 (Excel's leap-year bug is already in the serial)
    let (y, m, d) = civil_from_days(days - 25569);
    let secs = ((serial - serial.floor()) * 86400.0).round() as i64;
    if secs == 0 {
        format!("{d:02}/{m:02}/{y:04}")
    } else {
        format!("{d:02}/{m:02}/{y:04} {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    }
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// UTF-8 / UTF-16 (BOM) as text files, else Windows-874 (Thai Excel "CSV").
fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) || bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return super::text::decode(bytes);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes
            .iter()
            .map(|&b| match b {
                0x00..=0x7F => b as char,
                0xA1..=0xFB => char::from_u32(0x0E00 + (b as u32 - 0xA0)).unwrap_or('\u{FFFD}'),
                0xA0 => '\u{00A0}',
                0x85 => '…',
                0x91 | 0x92 => '\'',
                0x93 | 0x94 => '"',
                0x95 => '•',
                0x96 | 0x97 => '-',
                _ => '\u{FFFD}',
            })
            .collect(),
    }
}

fn read_delimited(bytes: &[u8]) -> Vec<Vec<String>> {
    let text = decode(bytes);
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let delim = [b'\t', b';', b',', b'|']
        .into_iter()
        .max_by_key(|d| first.bytes().filter(|b| b == d).count())
        .unwrap_or(b',');
    let mut r = csv::ReaderBuilder::new().has_headers(false).flexible(true).delimiter(delim).from_reader(text.as_bytes());
    r.records().filter_map(Result::ok).map(|rec| rec.iter().map(str::to_string).collect()).collect()
}

/// Values that carry nothing: empty, dashes, N/A, and zero durations like "00.00".
fn is_filler(v: &str) -> bool {
    let t = v.trim();
    t.is_empty()
        || t.chars().all(|c| c == '-' || c == '_' || c == '.')
        || t.eq_ignore_ascii_case("n/a")
        || t.eq_ignore_ascii_case("null")
        || t.eq_ignore_ascii_case("not specify")
        || (t.contains(['.', ':']) && t.chars().all(|c| c == '0' || c == '.' || c == ':'))
}

fn tidy(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A header wrapped inside its cell: "Name_Prefix⏎_Thai" → "Name_Prefix_Thai".
fn unwrap_header(h: &str) -> String {
    let mut out = String::new();
    for line in h.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !out.is_empty() && !out.ends_with('_') && !line.starts_with('_') {
            out.push(' ');
        }
        out.push_str(line);
    }
    tidy(&out)
}

fn blank(v: &str) -> bool {
    v.trim().is_empty()
}

fn numeric(v: &str) -> bool {
    v.trim().replace(',', "").parse::<f64>().is_ok()
}

/// Drop a leading group phrase of 3+ words that many headers share ("Customer and
/// Information Detail Incident No" → "Incident No"), unless that makes two headers equal.
/// "Many" = at least a tenth of the columns, so small groups like "Work around System
/// Name/Code" keep their full names.
pub fn clean_headers(headers: &[String]) -> Vec<String> {
    let min = (headers.len() / 10).max(3);
    let words: Vec<Vec<&str>> = headers.iter().map(|h| h.split_whitespace().collect()).collect();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for w in &words {
        for n in 3..w.len() {
            *counts.entry(w[..n].join(" ")).or_default() += 1;
        }
    }
    let shared = |p: &str| counts.get(p).copied().unwrap_or(0) >= min;
    let mut out: Vec<String> = Vec::with_capacity(headers.len());
    for (h, w) in headers.iter().zip(&words) {
        let Some(n) = (3..w.len()).rev().find(|&n| shared(&w[..n].join(" "))) else {
            out.push(tidy(h));
            continue;
        };
        let prefix = w[..n].join(" ");
        let mut rest: Vec<String> = w[n..].iter().map(|x| x.to_string()).collect();
        // "…Information Detailรายละเอียดปัญหา": the group's next word glued to the name
        if let Some(next) = counts
            .keys()
            .filter(|k| shared(k) && k.starts_with(&format!("{prefix} ")))
            .filter_map(|k| k[prefix.len() + 1..].split_whitespace().next().map(str::to_string))
            .find(|nw| rest[0].starts_with(nw.as_str()) && rest[0].len() > nw.len())
        {
            rest[0] = rest[0][next.len()..].to_string();
        }
        out.push(rest.join(" "));
    }
    // a shortened name that collides with another header keeps its full name
    let mut seen: HashMap<String, usize> = HashMap::new();
    for n in &out {
        *seen.entry(n.to_lowercase()).or_default() += 1;
    }
    out.iter()
        .zip(headers)
        .map(|(n, h)| if seen[&n.to_lowercase()] > 1 && tidy(h) != *n { tidy(h) } else { n.clone() })
        .collect()
}

/// Running-number columns ("No.", "ลำดับ") say nothing about the record.
fn is_counter(header: &str) -> bool {
    matches!(header.trim().to_lowercase().trim_end_matches('.'), "no" | "#" | "ลำดับ" | "ลำดับที่" | "row")
}

/// A short value that names the record: the first id-like column ("Incident No", "รหัส…").
fn record_id(headers: &[String], row: &[String]) -> Option<String> {
    let id_like = |h: &str| {
        let h = h.to_lowercase();
        ["no", "id", "code", "number", "รหัส", "เลขที่", "หมายเลข"].iter().any(|k| h.split(|c: char| !c.is_alphanumeric()).any(|w| w == *k) || h.contains(k) && !k.is_ascii())
    };
    headers
        .iter()
        .zip(row)
        .find(|(h, v)| !is_counter(h) && id_like(h) && !is_filler(v) && v.trim().chars().count() <= 40)
        .map(|(_, v)| tidy(v))
}

/// `row0`: Excel row of `rows[0]` minus one (a sheet whose data starts lower down).
fn rows_to_pages(label: &str, rows: &[Vec<String>], merges: &[Merge], row0: usize) -> Vec<Page> {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut grid: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let mut r = r.clone();
            r.resize(width, String::new());
            r
        })
        .collect();
    // a value merged down a column belongs to every row it covers (across columns it would
    // only repeat, so that is left to the header)
    for &((r0, c0), (r1, _)) in merges {
        let Some(v) = grid.get(r0).and_then(|r| r.get(c0)).cloned().filter(|v| !blank(v)) else { continue };
        for row in grid.iter_mut().take(r1 + 1).skip(r0 + 1) {
            if blank(&row[c0]) {
                row[c0] = v.clone();
            }
        }
    }
    tables(&grid).into_iter().flat_map(|(start, end)| table_pages(label, &grid, start, end, merges, row0)).collect()
}

/// (header row, end) of each table. A table starts at the first row with 2+ cells; after a
/// blank row, a row of short labels that uses columns the current header does not starts
/// the next one.
fn tables(rows: &[Vec<String>]) -> Vec<(usize, usize)> {
    let used = |r: &[String]| -> Vec<usize> { (0..r.len()).filter(|&c| !blank(&r[c])).collect() };
    let Some(mut start) = rows.iter().position(|r| used(r).len() >= 2) else { return Vec::new() };
    let mut head = used(&rows[start]);
    let mut out = Vec::new();
    for r in start + 1..rows.len() {
        let cols = used(&rows[r]);
        let header_like = cols.len() >= 2 && cols.iter().all(|&c| rows[r][c].trim().chars().count() <= 40 && !numeric(&rows[r][c]));
        if header_like
            && used(&rows[r - 1]).is_empty()
            && rows.get(r + 1).is_some_and(|n| !used(n).is_empty())
            && cols.iter().any(|c| !head.contains(c))
        {
            out.push((start, r));
            start = r;
            head = cols;
        }
    }
    out.push((start, rows.len()));
    out
}

fn table_pages(label: &str, grid: &[Vec<String>], start: usize, end: usize, merges: &[Merge], row0: usize) -> Vec<Page> {
    let width = grid[start].len();
    let cell = |r: usize, c: usize| grid[r][c].as_str();

    // header: a group merged across columns names each of them…
    let mut head: Vec<String> = grid[start].clone();
    let mut grouped = false;
    for &((r0, c0), (r1, c1)) in merges {
        if r0 <= start && start <= r1 && c1 > c0 && c0 < width {
            grouped = true;
            for h in head.iter_mut().take((c1 + 1).min(width)).skip(c0 + 1) {
                if blank(h) {
                    *h = cell(r0, c0).to_string();
                }
            }
        }
    }
    // …and the row under it, if it is labels too, holds the sub-headings
    let mut first = start + 1;
    if grouped && first < end {
        let sub = &grid[first];
        if sub.iter().filter(|v| !blank(v)).count() >= 2 && sub.iter().all(|v| blank(v) || !numeric(v)) {
            for (h, s) in head.iter_mut().zip(sub) {
                let (t, s) = (unwrap_header(h), unwrap_header(s));
                *h = if s.is_empty() || t == s {
                    t
                } else if t.is_empty() {
                    s
                } else {
                    format!("{t} {s}")
                };
            }
            first += 1;
        }
    }
    let headers = clean_headers(&head.iter().map(|h| unwrap_header(h)).collect::<Vec<_>>());
    let mut pages = Vec::new();

    // a field template laid out sideways: under a wide header, the first column holds a few
    // different labels ("คำอธิบาย", "ตัวอย่าง", "Mandatory") → one card per field
    let fc = (0..width).find(|&c| !headers[c].is_empty()).unwrap_or(0);
    let mut labels: Vec<(usize, String)> = Vec::new();
    for r in first..end {
        let v = tidy(cell(r, fc));
        // a value that comes back further down ("xxx" on every input row) is data, not a label
        let again = (r + 1..end).any(|k| tidy(cell(k, fc)) == v);
        if v.is_empty() || v.chars().count() > 40 || numeric(&v) || v == headers[fc] || again || labels.iter().any(|(_, l)| *l == v) {
            break;
        }
        labels.push((r, v));
    }
    let fields: Vec<usize> = (fc + 1..width).filter(|&c| !headers[c].is_empty()).collect();
    if (2..=8).contains(&labels.len()) && fields.len() + 1 >= (3 * labels.len()).max(6) {
        for &c in &fields {
            let lines: Vec<String> =
                labels.iter().filter(|(r, _)| !is_filler(cell(*r, c))).map(|(r, l)| format!("{l}: {}", tidy(cell(*r, c)))).collect();
            if !lines.is_empty() {
                for piece in pack(&format!("{label}คอลัมน์ {}", headers[c]), &lines) {
                    pages.push(Page::new(Some((start + 1 + row0) as u32), piece));
                }
            }
        }
        first = labels.last().map_or(first, |(r, _)| r + 1);
    }

    // a group written once over its rows ("1 ผู้จัดการร้าน", then blanks) fills down — only
    // left of the row's first value, and only in columns where groups repeat
    let regrouped = |c: usize| {
        let (mut seen, mut gap) = (false, false);
        for r in first..end {
            if !blank(cell(r, c)) {
                if seen && gap {
                    return true;
                }
                seen = true;
                gap = false;
            } else if seen && (0..width).any(|k| k != c && !blank(cell(r, k))) {
                gap = true;
            }
        }
        false
    };
    let fill: Vec<bool> = (0..width).map(|c| c >= fc && !headers[c].is_empty() && regrouped(c)).collect();
    let mut last: Vec<String> = vec![String::new(); width];
    let mut records: Vec<(usize, Option<String>, Vec<String>)> = Vec::new();

    for i in first..end {
        let mut row = grid[i].clone();
        if row.iter().all(|v| blank(v)) {
            last = vec![String::new(); width];
            continue;
        }
        let lead = row.iter().position(|v| !blank(v)).unwrap_or(width);
        for c in 0..lead {
            if fill[c] {
                row[c] = last[c].clone();
            }
        }
        for c in 0..width {
            if !blank(&row[c]) {
                last[c] = row[c].clone();
            }
        }
        let lines: Vec<String> = headers
            .iter()
            .zip(&row)
            .filter(|(h, v)| !is_filler(v) && !is_counter(h))
            .map(|(h, v)| if h.trim().is_empty() { tidy(v) } else { format!("{}: {}", h, tidy(v)) })
            .collect();
        if lines.is_empty() {
            continue;
        }
        records.push((i + 1 + row0, record_id(&headers, &row), lines));
    }

    // a lookup list (many short rows: code → name) reads best as a few rows together, so
    // "what values are there" finds the list and each piece still names its rows
    let mut sizes: Vec<usize> = records.iter().map(|(_, _, l)| l.iter().map(|x| x.chars().count() + 3).sum()).collect();
    sizes.sort_unstable();
    if records.len() >= 6 && sizes[sizes.len() / 2] <= 120 {
        let mut group: Vec<(usize, String)> = Vec::new();
        let flush = |group: &mut Vec<(usize, String)>, pages: &mut Vec<Page>| {
            let (Some(a), Some(b)) = (group.first().map(|g| g.0), group.last().map(|g| g.0)) else { return };
            let span = if a == b { format!("แถว {a}") } else { format!("แถว {a}–{b}") };
            let body: Vec<&str> = group.iter().map(|g| g.1.as_str()).collect();
            pages.push(Page::new(Some(a as u32), format!("{label}{span}\n{}", body.join("\n"))));
            group.clear();
        };
        let mut size = 0;
        for (r, _, lines) in &records {
            let line = format!("แถว {r}: {}", lines.join(" · "));
            if size + line.chars().count() > GROUP && !group.is_empty() {
                flush(&mut group, &mut pages);
                size = 0;
            }
            size += line.chars().count() + 1;
            group.push((*r, line));
        }
        flush(&mut group, &mut pages);
        return pages;
    }

    for (excel_row, id, lines) in records {
        let title = match id {
            Some(id) => format!("{label}แถว {excel_row} · {id}"),
            None => format!("{label}แถว {excel_row}"),
        };
        for piece in pack(&title, &lines) {
            pages.push(Page::new(Some(excel_row as u32), piece));
        }
    }
    pages
}

/// Cut a record into pieces that each start with `title` and stay under PIECE chars. The
/// Thai-aware chunker prefers line breaks, so short fields stay whole and long ones are cut
/// at a sentence or space (it may run up to ~25% over its size, hence the margin).
fn pack(title: &str, lines: &[String]) -> Vec<String> {
    let room = PIECE.saturating_sub(title.chars().count() + 1).max(200);
    let body = lines.join("
");
    if body.chars().count() <= room {
        return vec![format!("{title}
{body}")];
    }
    chunk(&body, ChunkOptions { size: room * 3 / 4, overlap: 0 })
        .into_iter()
        .map(|p| format!("{title}
{}", p.trim()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn shared_prefixes_go() {
        let h = s(&[
            "No.",
            "Customer and Information Detail Incident No",
            "Customer and Information Detail Status",
            "Customer and Information Detail บริษัท",
            "Work around Category",
            "Assign Category",
            "อีเมล์",
            "Customer and Information Detailรายละเอียดปัญหา",
            "Work around System Name",
            "Work around System Code",
        ]);
        assert_eq!(
            clean_headers(&h),
            s(&["No.", "Incident No", "Status", "บริษัท", "Work around Category", "Assign Category", "อีเมล์", "รายละเอียดปัญหา", "Work around System Name", "Work around System Code"])
        );
    }

    #[test]
    fn rows_become_titled_records() {
        let rows = vec![
            s(&["No.", "Customer and Information Detail Incident No", "Customer and Information Detail Status", "Customer and Information Detail รายละเอียดปัญหา", "Customer and Information Detail ใช้เวลา", "Assign Deadline"]),
            s(&["1", "2026/045161", "Finish", "ไม่สามารถ   เข้าสู่ระบบ app last mile", "00.00", "-"]),
            s(&["", "", "", "", "", ""]),
            s(&["3", "2026/045234", "Work around", &"ขาดงานทั้งวัน แต่หน้า New Time Sheet ขึ้นขาดงาน 1 ชั่วโมง ".repeat(40), "", ""]),
        ];
        let pages = rows_to_pages("", &rows, &[], 0);
        assert_eq!(pages[0].page, Some(2));
        assert_eq!(pages[0].text, "แถว 2 · 2026/045161\nIncident No: 2026/045161\nStatus: Finish\nรายละเอียดปัญหา: ไม่สามารถ เข้าสู่ระบบ app last mile");
        let long: Vec<&Page> = pages.iter().filter(|p| p.page == Some(4)).collect();
        assert!(long.len() >= 3, "a long row is split");
        assert!(long.iter().all(|p| p.text.starts_with("แถว 4 · 2026/045234\n") && p.text.chars().count() <= PIECE + 30));
    }

    #[test]
    fn csv_in_thai_windows_encoding() {
        // "รหัส,ชื่อ\n1001,สมชาย" in Windows-874
        let mut b = Vec::new();
        for c in "รหัส,ชื่อ\n1001,สมชาย".chars() {
            let u = c as u32;
            b.push(if (0x0E01..=0x0E5B).contains(&u) { (u - 0x0E00 + 0xA0) as u8 } else { u as u8 });
        }
        let rows = read_delimited(&b);
        assert_eq!(rows, vec![s(&["รหัส", "ชื่อ"]), s(&["1001", "สมชาย"])]);
        assert_eq!(rows_to_pages("", &rows, &[], 0)[0].text, "แถว 2 · 1001\nรหัส: 1001\nชื่อ: สมชาย");
    }

    #[test]
    fn excel_dates() {
        assert_eq!(excel_date(46336.0), "10/11/2026");
        assert_eq!(excel_date(46336.5), "10/11/2026 12:00:00");
    }

    #[test]
    fn merged_two_row_header() {
        // C1:C2 "No.", D1:E1 "Fields", F1:G1 "Database"
        let rows = vec![
            s(&["", "", "No.", "Fields", "", "Database", ""]),
            s(&["", "", "", "Fields", "Mandatory", "Table", "Field"]),
            s(&["", "", "1", "Employee_ID", "Yes", "Employee-cpa", "Employee_ID"]),
        ];
        let merges = [((0, 2), (1, 2)), ((0, 3), (0, 4)), ((0, 5), (0, 6))];
        let p = rows_to_pages("", &rows, &merges, 0);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].text, "แถว 3\nFields: Employee_ID\nFields Mandatory: Yes\nDatabase Table: Employee-cpa\nDatabase Field: Employee_ID");
    }

    #[test]
    fn sideways_template_becomes_field_cards() {
        let mut head = s(&["ชื่อฟิลด์"]);
        let mut desc = s(&["คำอธิบาย"]);
        let mut ex = s(&["ตัวอย่าง"]);
        let mut must = s(&["Mandatory"]);
        let mut data = s(&["xxx"]);
        for i in 0..8 {
            head.push(format!("Field{i}\n_Thai"));
            desc.push(format!("คำอธิบาย {i}"));
            ex.push(format!("ex{i}"));
            must.push(if i == 0 { "จำเป็นต้องกรอก".into() } else { "ไม่บังคับกรอก".into() });
            data.push(format!("v{i}"));
        }
        let p = rows_to_pages("", &[head, desc, ex, must, data.clone(), data], &[], 0);
        assert_eq!(p[0].text, "คอลัมน์ Field0_Thai\nคำอธิบาย: คำอธิบาย 0\nตัวอย่าง: ex0\nMandatory: จำเป็นต้องกรอก");
        assert_eq!(p.iter().filter(|x| x.text.starts_with("คอลัมน์ ")).count(), 8);
        let rec = &p[8];
        assert_eq!(rec.page, Some(5));
        assert!(rec.text.starts_with("แถว 5\nชื่อฟิลด์: xxx\nField0_Thai: v0"), "{}", rec.text);
    }

    #[test]
    fn stacked_tables_and_groups() {
        let rows = vec![
            s(&["", "Transaction_Type", "คำอธิบาย", ""]),
            s(&["", "1", "ข้อมูลพนักงานใหม่", ""]),
            s(&["", "", "", ""]),
            s(&["", "Role_ID", "Job Code", "หมายเหตุ"]),
            s(&["", "1 ผู้จัดการร้าน", "101318", ""]),
            s(&["", "", "101324", ""]),
            s(&["", "2 รักษาการ", "101337", ""]),
            s(&["", "", "101512", ""]),
        ];
        let t: Vec<String> = rows_to_pages("", &rows, &[], 0).into_iter().map(|p| p.text).collect();
        assert_eq!(t[0], "แถว 2\nTransaction_Type: 1\nคำอธิบาย: ข้อมูลพนักงานใหม่");
        assert_eq!(t[2], "แถว 6 · 1 ผู้จัดการร้าน\nRole_ID: 1 ผู้จัดการร้าน\nJob Code: 101324");
        assert_eq!(t[4], "แถว 8 · 2 รักษาการ\nRole_ID: 2 รักษาการ\nJob Code: 101512");
    }

    #[test]
    fn short_rows_are_grouped() {
        let mut rows = vec![s(&["Company_ID", "Company_Name"])];
        for i in 0..30 {
            rows.push(vec![format!("{}", 101 + i), format!("Company {i}")]);
        }
        let p = rows_to_pages("", &rows, &[], 0);
        assert!(p.len() >= 2 && p.len() <= 4, "{} pieces", p.len());
        assert_eq!(p[0].page, Some(2));
        assert!(p[0].text.starts_with("แถว 2–"));
        assert!(p[0].text.contains("\nแถว 2: Company_ID: 101 · Company_Name: Company 0\nแถว 3: Company_ID: 102"));
        assert!(p.iter().all(|x| x.text.chars().count() <= GROUP + 60));
    }

    #[test]
    fn lists_that_end_are_not_filled_down() {
        let rows = vec![s(&["Sex", "Company"]), s(&["Male", "101"]), s(&["Female", "102"]), s(&["", "103"])];
        let t: Vec<String> = rows_to_pages("", &rows, &[], 0).into_iter().map(|p| p.text).collect();
        assert_eq!(t[2], "แถว 4\nCompany: 103");
    }

    /// Does search find the right incident? `cargo test --release --lib sheet_search -- --ignored
    /// --nocapture` with DOCASK_TEST_FILE (incident report xlsx) and DOCASK_TEST_EMBED (e5-small folder).
    #[test]
    #[ignore]
    fn sheet_search() {
        use crate::store::{NewChunk, SqliteStore, VectorStore};
        let f = std::path::PathBuf::from(std::env::var("DOCASK_TEST_FILE").expect("DOCASK_TEST_FILE"));
        let emb = std::path::PathBuf::from(std::env::var("DOCASK_TEST_EMBED").expect("DOCASK_TEST_EMBED"));
        let enc = crate::embed::Embedder::load(crate::embed::ModelId::MultilingualE5Small, &emb).unwrap();
        let ex = super::super::extract_chunks(&f, ChunkOptions::default()).unwrap();
        let texts: Vec<String> = ex.chunks.iter().map(|(_, t)| format!("passage: {t}")).collect();
        let vecs = enc.embed(&texts, crate::encoder::Pooling::Mean, 8).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut st = SqliteStore::open(&tmp.path().join("i.db")).unwrap();
        let id = st.upsert_document(&f.to_string_lossy(), "h", 1, 1, "indexing", None).unwrap();
        let chunks: Vec<NewChunk> = ex.chunks.iter().zip(vecs).enumerate()
            .map(|(i, ((page, text), e))| NewChunk { page: *page, ord: i as i64, text: text.clone(), embedding: e }).collect();
        st.replace_chunks(id, &chunks, &[]).unwrap();
        let qs: [(&str, &[u32]); 9] = [
            ("ปัญหา Login App Staff Delivery ขึ้น Employee is not active แก้ยังไง", &[2, 3]),
            ("รันรายงานภาษี เลขบัตรประชาชนแสดงเป็นดอกจัน", &[6]),
            ("เคส 2026/045234 สาเหตุคืออะไร", &[4]),
            ("ผู้ช่วยผู้จัดการร้านเมนูผู้จัดการไม่ขึ้นใน HR Connect", &[5]),
            ("เกรด PMS ไม่ส่งไประบบ SAP", &[8]),
            ("ขาดงานทั้งวันแต่ time sheet แสดงขาดงาน 1 ชั่วโมง", &[4]),
            ("แบบประเมิน ALL PERFORM Plus หายไปจากระบบ", &[7]),
            ("HR ดึงข้อมูลดูแลบุตรคลอดใหม่ไม่ได้", &[10]),
            ("รหัสพนักงาน 0731634 แจ้งปัญหาอะไร", &[3]),
        ];
        let (mut top1, mut top3) = (0, 0);
        for (q, want) in qs {
            let qv = enc.embed(&[format!("query: {q}")], crate::encoder::Pooling::Mean, 1).unwrap().pop().unwrap();
            let hits = st.hybrid_search(&qv, q, 5, 0.3).unwrap();
            let rows: Vec<u32> = hits.iter().filter_map(|h| h.page).collect();
            let mut uniq: Vec<u32> = Vec::new();
            for r in rows { if !uniq.contains(&r) { uniq.push(r); } }
            let ok1 = uniq.first().is_some_and(|r| want.contains(r));
            let ok3 = uniq.iter().take(3).any(|r| want.contains(r));
            top1 += ok1 as usize;
            top3 += ok3 as usize;
            println!("{} {q}  → rows {:?} (want {want:?})", if ok1 { "✓" } else if ok3 { "~" } else { "✗" }, &uniq[..uniq.len().min(4)]);
        }
        println!("right incident first: {top1}/9 · in top-3 rows: {top3}/9");
    }

    /// Search any spreadsheet: DOCASK_TEST_FILE, DOCASK_TEST_EMBED and DOCASK_TEST_QUERIES (a
    /// UTF-8 file of "question<TAB>text the right piece contains" lines).
    #[test]
    #[ignore]
    fn sheet_ask() {
        use crate::store::{NewChunk, SqliteStore, VectorStore};
        let env = |k: &str| std::path::PathBuf::from(std::env::var(k).expect(k));
        let (f, emb) = (env("DOCASK_TEST_FILE"), env("DOCASK_TEST_EMBED"));
        let qs = std::fs::read_to_string(env("DOCASK_TEST_QUERIES")).unwrap();
        let enc = crate::embed::Embedder::load(crate::embed::ModelId::MultilingualE5Small, &emb).unwrap();
        let ex = super::super::extract_chunks(&f, ChunkOptions::default()).unwrap();
        let texts: Vec<String> = ex.chunks.iter().map(|(_, t)| format!("passage: {t}")).collect();
        let vecs = enc.embed(&texts, crate::encoder::Pooling::Mean, 8).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut st = SqliteStore::open(&tmp.path().join("i.db")).unwrap();
        let id = st.upsert_document(&f.to_string_lossy(), "h", 1, 1, "indexing", None).unwrap();
        let chunks: Vec<NewChunk> = ex.chunks.iter().zip(vecs).enumerate()
            .map(|(i, ((page, text), e))| NewChunk { page: *page, ord: i as i64, text: text.clone(), embedding: e }).collect();
        st.replace_chunks(id, &chunks, &[]).unwrap();
        let (mut n, mut top1, mut top5) = (0, 0, 0);
        for line in qs.lines().filter(|l| l.contains('\t')) {
            let (q, want) = line.split_once('\t').unwrap();
            let qv = enc.embed(&[format!("query: {q}")], crate::encoder::Pooling::Mean, 1).unwrap().pop().unwrap();
            let hits = st.hybrid_search(&qv, q, 5, 0.3).unwrap();
            let at = hits.iter().position(|h| h.text.contains(want.trim()));
            n += 1;
            top1 += (at == Some(0)) as usize;
            top5 += at.is_some() as usize;
            let first = hits.first().map(|h| h.text.lines().next().unwrap_or("").to_string()).unwrap_or_default();
            println!("{} {q}  → #{} · top: {first}", match at { Some(0) => "✓", Some(_) => "~", None => "✗" }, at.map_or("-".into(), |a| (a + 1).to_string()));
        }
        println!("first: {top1}/{n} · in top-5: {top5}/{n}");
    }

    /// `cargo test --lib sheet_real -- --ignored --nocapture` with DOCASK_TEST_FILE=<xlsx>
    #[test]
    #[ignore]
    fn sheet_real() {
        let f = std::path::PathBuf::from(std::env::var("DOCASK_TEST_FILE").expect("DOCASK_TEST_FILE"));
        let pages = SheetExtractor.extract(&f).unwrap();
        println!("{} pieces", pages.len());
        for p in &pages {
            println!("----- row {:?} ({} chars)\n{}", p.page, p.text.chars().count(), p.text);
        }
    }
}
