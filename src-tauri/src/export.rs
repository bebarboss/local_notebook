//! Turn a chat answer (Markdown) into a downloadable file.
//!
//! Four formats, all built in pure Rust (no extra crates): `.md` keeps the answer as
//! written, `.txt` strips the Markdown to readable plain text, and `.csv` / `.xlsx`
//! pull the Markdown tables out of the answer (falling back to one line per row when the
//! answer has no table). The `.xlsx` is a minimal, valid OOXML workbook written with the
//! `zip` crate and inline strings — enough for Excel to open it with Thai intact.
//!
//! Privacy: this never logs the answer text; it only transforms bytes the caller passed.

use crate::errors::{AppError, AppResult};
use once_cell::sync::Lazy;
use regex::Regex;
use std::io::Write;

/// Build the file bytes for `content` (the answer Markdown) in `format`
/// (`"md" | "txt" | "csv" | "xlsx"`).
pub fn build(content: &str, format: &str) -> AppResult<Vec<u8>> {
    match format {
        "md" => Ok(content.as_bytes().to_vec()),
        "txt" => Ok(to_plain(content).into_bytes()),
        "csv" => Ok(csv_bytes(content)),
        "xlsx" => xlsx_bytes(content),
        _ => Err(AppError::new("export", "รูปแบบไฟล์ไม่รองรับ")),
    }
}

// ----------------------------------------------------------------- plain text

static LINK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap());

/// A readable plain-text rendering of the answer: links collapse to their text, bold /
/// code markers drop, headings lose their `#`, and bullets become `• `. Table separator
/// rows (`| --- |`) are removed but the data rows are kept as-is (they read fine).
pub fn to_plain(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    for line in md.lines() {
        let line = line.trim_end();
        if is_separator(line) {
            continue;
        }
        let mut s = LINK.replace_all(line, "$1").into_owned();
        s = s.replace("**", "").replace("__", "").replace('`', "");
        let body = s.trim_start();
        let body = body.trim_start_matches(['#', '>']).trim_start();
        let body = match body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
            Some(rest) => format!("• {rest}"),
            None => body.to_string(),
        };
        out.push_str(&body);
        out.push('\n');
    }
    out
}

// --------------------------------------------------------------- table parsing

/// Split the answer into its Markdown tables. Each table is a list of rows; each row a list
/// of trimmed cells. A table is a `|`-row immediately followed by a separator row.
fn parse_tables(md: &str) -> Vec<Vec<Vec<String>>> {
    let lines: Vec<&str> = md.lines().collect();
    let mut tables = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if is_row(lines[i]) && i + 1 < lines.len() && is_separator(lines[i + 1]) {
            let mut rows = vec![split_row(lines[i])];
            let mut j = i + 2;
            while j < lines.len() && is_row(lines[j]) {
                rows.push(split_row(lines[j]));
                j += 1;
            }
            tables.push(rows);
            i = j;
        } else {
            i += 1;
        }
    }
    tables
}

/// A line that could be a table row: has a `|` and some non-pipe content.
fn is_row(line: &str) -> bool {
    let t = line.trim();
    t.contains('|') && t.chars().any(|c| c != '|' && !c.is_whitespace())
}

/// A table separator row like `| --- | :--: |` — cells of only `-`, `:` and spaces.
fn is_separator(line: &str) -> bool {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    if t.trim().is_empty() {
        return false;
    }
    t.split('|').all(|cell| {
        let c = cell.trim();
        !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')
    })
}

/// Cells of a table row, handling `\|` as a literal pipe and dropping the empty cells the
/// leading / trailing `|` produce.
fn split_row(line: &str) -> Vec<String> {
    let holder = '\u{0}';
    let masked = line.trim().replace("\\|", &holder.to_string());
    let trimmed = masked.trim_start_matches('|').trim_end_matches('|');
    trimmed
        .split('|')
        .map(|c| c.trim().replace(holder, "|"))
        .collect()
}

/// The rows to put in a CSV / XLSX: the answer's tables (stacked, blank row between), or —
/// when there is no table — one single-cell row per non-empty plain-text line.
fn table_rows(content: &str) -> Vec<Vec<String>> {
    let tables = parse_tables(content);
    if tables.is_empty() {
        return to_plain(content)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| vec![l.to_string()])
            .collect();
    }
    let mut rows = Vec::new();
    for (i, t) in tables.iter().enumerate() {
        if i > 0 {
            rows.push(vec![String::new()]);
        }
        rows.extend(t.iter().cloned());
    }
    rows
}

// ------------------------------------------------------------------------ csv

fn csv_bytes(content: &str) -> Vec<u8> {
    let rows = table_rows(content);
    let mut w = csv::WriterBuilder::new().flexible(true).from_writer(Vec::new());
    for r in &rows {
        let _ = w.write_record(r);
    }
    // UTF-8 BOM so Excel opens Thai text in the right encoding.
    let mut out = vec![0xEF, 0xBB, 0xBF];
    out.extend(w.into_inner().unwrap_or_default());
    out
}

// ----------------------------------------------------------------------- xlsx

fn zip_err(e: zip::result::ZipError) -> AppError {
    AppError::new("zip", format!("สร้างไฟล์ Excel ไม่สำเร็จ: {e}"))
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;

const WORKBOOK: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="คำตอบ" sheetId="1" r:id="rId1"/></sheets></workbook>"#;

const WB_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;

fn xlsx_bytes(content: &str) -> AppResult<Vec<u8>> {
    let rows = table_rows(content);
    let buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for (name, body) in [
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", RELS),
        ("xl/workbook.xml", WORKBOOK),
        ("xl/_rels/workbook.xml.rels", WB_RELS),
    ] {
        zip.start_file(name, opts).map_err(zip_err)?;
        zip.write_all(body.as_bytes())?;
    }
    zip.start_file("xl/worksheets/sheet1.xml", opts).map_err(zip_err)?;
    zip.write_all(sheet_xml(&rows).as_bytes())?;

    Ok(zip.finish().map_err(zip_err)?.into_inner())
}

fn sheet_xml(rows: &[Vec<String>]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
    );
    for (ri, row) in rows.iter().enumerate() {
        let rn = ri + 1;
        s.push_str(&format!("<row r=\"{rn}\">"));
        for (ci, cell) in row.iter().enumerate() {
            let cref = format!("{}{}", col_name(ci), rn);
            if is_number(cell) {
                s.push_str(&format!("<c r=\"{cref}\"><v>{}</v></c>", cell.trim()));
            } else if cell.is_empty() {
                s.push_str(&format!("<c r=\"{cref}\"/>"));
            } else {
                let space = if cell != cell.trim() { " xml:space=\"preserve\"" } else { "" };
                s.push_str(&format!("<c r=\"{cref}\" t=\"inlineStr\"><is><t{space}>{}</t></is></c>", esc(cell)));
            }
        }
        s.push_str("</row>");
    }
    s.push_str("</sheetData></worksheet>");
    s
}

/// 0-based column index to its Excel letters (0→A, 25→Z, 26→AA).
fn col_name(mut n: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

static NUMBER: Lazy<Regex> = Lazy::new(|| Regex::new(r"^-?(0|[1-9]\d{0,14})(\.\d+)?$").unwrap());

/// Whether a cell should be written as a number. Integers are capped at 15 digits and
/// leading zeros are rejected so ids, codes and phone numbers stay text.
fn is_number(cell: &str) -> bool {
    NUMBER.is_match(cell.trim())
}

/// Escape text for an XML element body, dropping characters XML forbids.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => o.push(c),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = "สรุปปัญหา **Store Time Management** ตามเอกสาร [1]\n\n\
| ปัญหา | สาเหตุ | วิธีแก้ |\n\
| --- | --- | --- |\n\
| ปุ่มแก้ไข JOB เป็นสีเทา | สิทธิ์ไม่พอ | เปิดสิทธิ์ JOB |\n\
| Login ค้าง | session เต็ม | ล้าง session |\n\n\
ดูรายละเอียดเพิ่มที่ [คู่มือ](https://x/y) ได้";

    #[test]
    fn plain_text_strips_markdown_and_keeps_rows() {
        let t = to_plain(ANSWER);
        assert!(t.contains("Store Time Management") && !t.contains("**"));
        // the link collapses to its text, the URL drops
        assert!(t.contains("คู่มือ") && !t.contains("https://"));
        // the separator row is gone but data rows remain
        assert!(!t.contains("---"));
        assert!(t.contains("ปุ่มแก้ไข JOB เป็นสีเทา"));
    }

    #[test]
    fn parses_the_markdown_table() {
        let tables = parse_tables(ANSWER);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].len(), 3, "header + 2 rows");
        assert_eq!(tables[0][0], vec!["ปัญหา", "สาเหตุ", "วิธีแก้"]);
        assert_eq!(tables[0][2][0], "Login ค้าง");
    }

    #[test]
    fn csv_has_bom_and_the_table() {
        let b = csv_bytes(ANSWER);
        assert_eq!(&b[..3], &[0xEF, 0xBB, 0xBF], "UTF-8 BOM for Excel");
        let text = String::from_utf8(b[3..].to_vec()).unwrap();
        assert!(text.contains("ปัญหา,สาเหตุ,วิธีแก้"));
        assert!(text.contains("Login ค้าง"));
    }

    #[test]
    fn xlsx_is_a_zip_with_the_parts() {
        let b = xlsx_bytes(ANSWER).unwrap();
        assert_eq!(&b[..2], b"PK", "a zip (OOXML) container");
        // the parts are deflated, so read them back to confirm the sheet holds the data
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(b)).unwrap();
        let names: Vec<String> = (0..z.len()).map(|i| z.by_index(i).unwrap().name().to_string()).collect();
        assert!(names.contains(&"xl/worksheets/sheet1.xml".to_string()));
        use std::io::Read;
        let mut sheet = String::new();
        z.by_name("xl/worksheets/sheet1.xml").unwrap().read_to_string(&mut sheet).unwrap();
        assert!(sheet.contains("ปัญหา"));
        assert!(sheet.contains("Login ค้าง"));
    }

    #[test]
    fn no_table_falls_back_to_lines() {
        let rows = table_rows("บรรทัดแรก\n\nบรรทัดสอง");
        assert_eq!(rows, vec![vec!["บรรทัดแรก".to_string()], vec!["บรรทัดสอง".to_string()]]);
    }

    #[test]
    fn numbers_are_numeric_but_ids_stay_text() {
        assert!(is_number("12") && is_number("-3.5") && is_number("0"));
        assert!(!is_number("007") && !is_number("1234567890123456") && !is_number("08:30") && !is_number(""));
    }

    #[test]
    fn column_letters_roll_over() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(27), "AB");
    }

    #[test]
    fn escapes_xml_special_characters() {
        let x = sheet_xml(&[vec!["a<b>&\"c".to_string()]]);
        assert!(x.contains("a&lt;b&gt;&amp;&quot;c"));
    }

    #[test]
    fn unknown_format_is_rejected() {
        assert!(build("x", "pdf").is_err());
        assert_eq!(build("# hi", "md").unwrap(), b"# hi");
    }

    #[test]
    fn escaped_pipes_stay_in_the_cell() {
        let md = "| a | b |\n| --- | --- |\n| x \\| y | z |";
        let t = parse_tables(md);
        assert_eq!(t[0][1], vec!["x | y", "z"]);
    }
}
