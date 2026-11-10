//! Services = groups of documents, each with its own index file.

use crate::config::{data_dir, read_json, write_json};
use crate::errors::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub id: String,
    pub name: String,
    pub code: String,
    pub color: String,
    #[serde(default)]
    pub folders: Vec<String>,
    /// individual files added with "+ เพิ่ม" (outside the watched folders)
    #[serde(default)]
    pub files: Vec<String>,
    pub model: String,
    pub effort: String,
    pub top_k: u32,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewService {
    pub name: String,
    pub code: String,
    pub color: String,
    #[serde(default)]
    pub folders: Vec<String>,
    pub model: String,
    pub effort: String,
    pub top_k: u32,
    #[serde(default)]
    pub system_prompt: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStats {
    pub service_id: String,
    pub documents: i64,
    pub chunks: i64,
    pub bytes: i64,
    pub skipped: i64,
    pub indexing: i64,
    pub last_indexed_at: Option<i64>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn services_root() -> PathBuf {
    data_dir().join("services")
}

pub fn service_dir(id: &str) -> PathBuf {
    services_root().join(id)
}

pub fn index_path(id: &str) -> PathBuf {
    service_dir(id).join("index.db")
}

fn list_path() -> PathBuf {
    data_dir().join("services.json")
}

pub struct ServiceRegistry {
    items: Vec<Service>,
}

impl ServiceRegistry {
    pub fn load() -> Self {
        Self { items: read_json(&list_path()).ok().flatten().unwrap_or_default() }
    }

    fn save(&self) -> AppResult<()> {
        write_json(&list_path(), &self.items)
    }

    pub fn list(&self) -> Vec<Service> {
        self.items.clone()
    }

    pub fn get(&self, id: &str) -> AppResult<Service> {
        self.items.iter().find(|s| s.id == id).cloned().ok_or_else(|| AppError::new("not_found", "ไม่พบ service นี้"))
    }

    pub fn create(&mut self, n: NewService) -> AppResult<Service> {
        let (name, code, color, effort) = validate(&n.name, &n.code, &n.color, &n.effort)?;
        let now = now_ms();
        let s = Service {
            id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
            name,
            code,
            color,
            folders: dedup(n.folders),
            files: vec![],
            model: n.model,
            effort,
            top_k: n.top_k.clamp(1, 30),
            system_prompt: n.system_prompt.trim().to_string(),
            created_at: now,
            updated_at: now,
        };
        std::fs::create_dir_all(service_dir(&s.id))?;
        self.items.push(s.clone());
        self.save()?;
        Ok(s)
    }

    pub fn update(&mut self, s: Service) -> AppResult<Service> {
        let (name, code, color, effort) = validate(&s.name, &s.code, &s.color, &s.effort)?;
        let cur = self.items.iter_mut().find(|x| x.id == s.id).ok_or_else(|| AppError::new("not_found", "ไม่พบ service นี้"))?;
        cur.name = name;
        cur.code = code;
        cur.color = color;
        cur.effort = effort;
        cur.folders = dedup(s.folders);
        cur.files = dedup(s.files);
        cur.model = s.model;
        cur.top_k = s.top_k.clamp(1, 30);
        cur.system_prompt = s.system_prompt.trim().to_string();
        cur.updated_at = now_ms();
        let out = cur.clone();
        self.save()?;
        Ok(out)
    }

    pub fn add_files(&mut self, id: &str, files: &[String]) -> AppResult<Service> {
        let cur = self.items.iter_mut().find(|x| x.id == id).ok_or_else(|| AppError::new("not_found", "ไม่พบ service นี้"))?;
        for f in files {
            if !cur.files.iter().any(|x| x.eq_ignore_ascii_case(f)) {
                cur.files.push(f.clone());
            }
        }
        cur.updated_at = now_ms();
        let out = cur.clone();
        self.save()?;
        Ok(out)
    }

    pub fn remove_file(&mut self, id: &str, path: &str) -> AppResult<()> {
        if let Some(cur) = self.items.iter_mut().find(|x| x.id == id) {
            cur.files.retain(|f| !f.eq_ignore_ascii_case(path));
        }
        self.save()
    }

    pub fn touch(&mut self, id: &str) {
        if let Some(cur) = self.items.iter_mut().find(|x| x.id == id) {
            cur.updated_at = now_ms();
        }
        let _ = self.save();
    }

    pub fn delete(&mut self, id: &str) -> AppResult<()> {
        self.items.retain(|s| s.id != id);
        self.save()?;
        let dir = service_dir(id);
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

fn dedup(mut v: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    v.retain(|x| !x.trim().is_empty() && seen.insert(x.to_lowercase()));
    v
}

fn validate(name: &str, code: &str, color: &str, effort: &str) -> AppResult<(String, String, String, String)> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(AppError::new("invalid", "กรุณาตั้งชื่อ service (ไม่เกิน 80 ตัวอักษร)"));
    }
    let code = code.trim().to_uppercase();
    let n = code.chars().count();
    if !(2..=3).contains(&n) || code.chars().any(char::is_whitespace) {
        return Err(AppError::new("invalid", "รหัสย่อต้องมี 2–3 ตัวอักษร"));
    }
    let color = color.trim();
    let ok_color = color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !ok_color {
        return Err(AppError::new("invalid", "สีต้องอยู่ในรูปแบบ #RRGGBB"));
    }
    let effort = match effort {
        "low" | "medium" | "high" => effort,
        _ => "medium",
    };
    Ok((name.to_string(), code, color.to_uppercase(), effort.to_string()))
}
