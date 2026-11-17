//! Regiões do canvas persistidas em `~/.config/spatial-deck/regions.json`.

use std::{fs, path::PathBuf};

use common::CanvasRegion;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Persisted {
    #[serde(default)]
    regions: Vec<CanvasRegion>,
}

pub struct RegionStore {
    next_id: u32,
    regions: Vec<CanvasRegion>,
    /// `None` em testes: nada é gravado em disco.
    path: Option<PathBuf>,
}

impl RegionStore {
    pub fn load() -> Self {
        let path = dirs::config_dir().map(|dir| dir.join("spatial-deck").join("regions.json"));
        let regions = path
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Persisted>(&text).ok())
            .map(|persisted| persisted.regions)
            .unwrap_or_default();
        let next_id = regions
            .iter()
            .map(|region| region.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        Self {
            next_id,
            regions,
            path,
        }
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            next_id: 1,
            regions: Vec::new(),
            path: None,
        }
    }

    pub fn list(&self) -> Vec<CanvasRegion> {
        self.regions.clone()
    }

    /// Cria uma região com a transformação informada. Nome vazio vira
    /// "Região N" para não deixar chip sem rótulo.
    pub fn save(&mut self, name: &str, scale: f64, offset_x: f64, offset_y: f64) -> CanvasRegion {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1).max(1);
        let name = name.trim();
        let region = CanvasRegion {
            id,
            name: if name.is_empty() {
                format!("Região {id}")
            } else {
                name.to_owned()
            },
            scale,
            offset_x,
            offset_y,
        };
        self.regions.push(region.clone());
        self.persist();
        region
    }

    pub fn delete(&mut self, id: u32) -> bool {
        let before = self.regions.len();
        self.regions.retain(|region| region.id != id);
        let removed = before != self.regions.len();
        if removed {
            self.persist();
        }
        removed
    }

    fn persist(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let data = Persisted {
            regions: self.regions.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&data) {
            let _ = fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_sequential_ids() {
        let mut store = RegionStore::in_memory();
        let first = store.save("A", 1.0, 10.0, 20.0);
        let second = store.save("B", 2.0, -30.0, 40.0);
        assert_eq!(first.id, 1);
        assert_eq!(second.id, 2);
        assert_eq!(store.list().len(), 2);
    }

    #[test]
    fn empty_name_gets_default() {
        let mut store = RegionStore::in_memory();
        let region = store.save("   ", 1.0, 0.0, 0.0);
        assert_eq!(region.name, "Região 1");
    }

    #[test]
    fn delete_removes_and_keeps_sequence() {
        let mut store = RegionStore::in_memory();
        let first = store.save("A", 1.0, 0.0, 0.0);
        store.save("B", 1.0, 0.0, 0.0);
        assert!(store.delete(first.id));
        assert!(!store.delete(first.id));
        assert_eq!(store.list().len(), 1);
        let next = store.save("C", 1.0, 0.0, 0.0);
        assert_eq!(next.id, 3);
    }

    #[test]
    fn keeps_transform_values() {
        let mut store = RegionStore::in_memory();
        let region = store.save("zoom", 1.75, -120.5, 33.25);
        let listed = &store.list()[0];
        assert_eq!(listed.scale, 1.75);
        assert_eq!(listed.offset_x, -120.5);
        assert_eq!(listed.offset_y, 33.25);
        assert_eq!(region.id, listed.id);
    }
}
