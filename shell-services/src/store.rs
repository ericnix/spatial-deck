//! Armazenamento das notificações: ativas (com prazo), histórico e DND.

use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use common::NotificationItem;

/// Quantas notificações o histórico mantém.
pub const HISTORY_LIMIT: usize = 100;

struct Active {
    item: NotificationItem,
    deadline: Option<Instant>,
    /// Tempo restante congelado enquanto o ponteiro está sobre o popup.
    hold_remaining: Option<std::time::Duration>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Persisted {
    #[serde(default)]
    dnd: bool,
    #[serde(default)]
    items: Vec<NotificationItem>,
}

pub struct Store {
    next_id: u32,
    active: Vec<Active>,
    history: VecDeque<NotificationItem>,
    dnd: bool,
    /// `None` em testes: nada é gravado em disco.
    path: Option<PathBuf>,
}

impl Store {
    /// Carrega histórico e DND de `~/.config/spatial-deck/notifications.json`.
    pub fn load() -> Self {
        let path = dirs::config_dir().map(|dir| dir.join("spatial-deck").join("notifications.json"));
        let persisted = path
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Persisted>(&text).ok())
            .unwrap_or_default();

        let next_id = persisted
            .items
            .iter()
            .map(|item| item.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        let mut history: VecDeque<NotificationItem> = persisted.items.into_iter().collect();
        while history.len() > HISTORY_LIMIT {
            history.pop_back();
        }

        Self {
            next_id,
            active: Vec::new(),
            history,
            dnd: persisted.dnd,
            path,
        }
    }

    /// Store sem persistência, para testes.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            next_id: 1,
            active: Vec::new(),
            history: VecDeque::new(),
            dnd: false,
            path: None,
        }
    }

    /// Registra (ou substitui) uma notificação e devolve o id final.
    pub fn push(
        &mut self,
        mut item: NotificationItem,
        replaces_id: u32,
        deadline: Option<Instant>,
    ) -> u32 {
        let replace = replaces_id > 0
            && self
                .active
                .iter()
                .any(|entry| entry.item.id == replaces_id);
        let id = if replace {
            replaces_id
        } else {
            let id = self.next_id;
            self.next_id = self.next_id.saturating_add(1).max(1);
            id
        };

        item.id = id;
        item.timestamp_ms = now_ms();

        self.active.retain(|entry| entry.item.id != id);
        self.active.push(Active {
            item: item.clone(),
            deadline,
            hold_remaining: None,
        });

        self.history.retain(|entry| entry.id != id);
        self.history.push_front(item);
        self.history.truncate(HISTORY_LIMIT);
        self.persist();
        id
    }

    /// Remove uma notificação ativa. `true` se ela existia.
    pub fn close(&mut self, id: u32) -> bool {
        let before = self.active.len();
        self.active.retain(|entry| entry.item.id != id);
        before != self.active.len()
    }

    /// Congela/retoma o prazo de uma notificação (hover no popup).
    pub fn hold(&mut self, id: u32, held: bool) {
        let Some(entry) = self.active.iter_mut().find(|entry| entry.item.id == id) else {
            return;
        };
        if held {
            if let Some(deadline) = entry.deadline.take() {
                entry.hold_remaining = Some(deadline.saturating_duration_since(Instant::now()));
            }
        } else if let Some(remaining) = entry.hold_remaining.take() {
            entry.deadline = Some(Instant::now() + remaining);
        }
    }

    /// Ids cujo prazo venceu; remove-os das ativas.
    pub fn expire_due(&mut self, now: Instant) -> Vec<u32> {
        let mut expired = Vec::new();
        self.active.retain(|entry| {
            if entry.deadline.is_some_and(|deadline| deadline <= now) {
                expired.push(entry.item.id);
                false
            } else {
                true
            }
        });
        expired
    }

    pub fn history(&self) -> Vec<NotificationItem> {
        self.history.iter().cloned().collect()
    }

    pub fn set_dnd(&mut self, active: bool) {
        if self.dnd != active {
            self.dnd = active;
            self.persist();
        }
    }

    pub fn dnd(&self) -> bool {
        self.dnd
    }

    #[cfg(test)]
    pub fn active_len(&self) -> usize {
        self.active.len()
    }

    fn persist(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let data = Persisted {
            dnd: self.dnd,
            items: self.history.iter().cloned().collect(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&data) {
            let _ = fs::write(path, json);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::NotificationButton;

    fn item(summary: &str) -> NotificationItem {
        NotificationItem {
            id: 0,
            app_name: "teste".into(),
            summary: summary.into(),
            body: String::new(),
            icon: String::new(),
            urgency: 1,
            actions: Vec::new(),
            timestamp_ms: 0,
        }
    }

    #[test]
    fn assigns_sequential_ids() {
        let mut store = Store::in_memory();
        let first = store.push(item("a"), 0, None);
        let second = store.push(item("b"), 0, None);
        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(store.active_len(), 2);
        assert_eq!(store.history().len(), 2);
    }

    #[test]
    fn replaces_keeps_id_and_does_not_duplicate() {
        let mut store = Store::in_memory();
        let id = store.push(item("a"), 0, None);
        let replaced = store.push(item("b"), id, None);
        assert_eq!(replaced, id);
        assert_eq!(store.active_len(), 1);
        assert_eq!(store.history().len(), 1);
        assert_eq!(store.history()[0].summary, "b");
    }

    #[test]
    fn close_removes_only_active() {
        let mut store = Store::in_memory();
        let id = store.push(item("a"), 0, None);
        assert!(store.close(id));
        assert!(!store.close(id));
        assert_eq!(store.active_len(), 0);
        // O histórico continua guardando a notificação fechada.
        assert_eq!(store.history().len(), 1);
    }

    #[test]
    fn expiry_respects_deadline() {
        let mut store = Store::in_memory();
        let now = Instant::now();
        let id = store.push(item("a"), 0, Some(now));
        assert_eq!(store.expire_due(now - std::time::Duration::from_millis(1)), Vec::<u32>::new());
        assert_eq!(store.expire_due(now), vec![id]);
        assert_eq!(store.active_len(), 0);
    }

    #[test]
    fn hold_pauses_and_resumes_expiry() {
        let mut store = Store::in_memory();
        let now = Instant::now();
        let id = store.push(item("a"), 0, Some(now));
        store.hold(id, true);
        assert_eq!(store.expire_due(now + std::time::Duration::from_secs(5)), Vec::<u32>::new());
        store.hold(id, false);
        assert_eq!(store.expire_due(Instant::now() + std::time::Duration::from_secs(1)), vec![id]);
    }

    #[test]
    fn history_is_limited() {
        let mut store = Store::in_memory();
        for index in 0..(HISTORY_LIMIT + 7) {
            store.push(item(&format!("notificação {index}")), 0, None);
        }
        assert_eq!(store.history().len(), HISTORY_LIMIT);
        assert_eq!(store.history()[0].summary, format!("notificação {}", HISTORY_LIMIT + 6));
    }

    #[test]
    fn dnd_toggles() {
        let mut store = Store::in_memory();
        assert!(!store.dnd());
        store.set_dnd(true);
        assert!(store.dnd());
        store.set_dnd(false);
        assert!(!store.dnd());
    }

    #[test]
    fn buttons_survive_the_store() {
        let mut store = Store::in_memory();
        let mut notification = item("com ação");
        notification.actions = vec![NotificationButton {
            key: "default".into(),
            label: "Abrir".into(),
        }];
        store.push(notification, 0, None);
        assert_eq!(store.history()[0].actions[0].key, "default");
    }
}
