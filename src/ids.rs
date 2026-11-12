use std::collections::HashMap;
use std::fs;
use std::sync::OnceLock;

pub fn user_map() -> &'static HashMap<u32, String> {
    static MAP: OnceLock<HashMap<u32, String>> = OnceLock::new();
    MAP.get_or_init(|| parse_id_map("/etc/passwd"))
}

pub fn group_map() -> &'static HashMap<u32, String> {
    static MAP: OnceLock<HashMap<u32, String>> = OnceLock::new();
    MAP.get_or_init(|| parse_id_map("/etc/group"))
}

pub fn parse_id_map(path: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();

    if let Ok(contents) = fs::read_to_string(path) {
        for line in contents.lines() {
            let mut fields = line.split(':');
            let name = match fields.next() {
                Some(name) if !name.is_empty() => name,
                _ => continue,
            };
            let _password = fields.next();
            let id = match fields.next().and_then(|field| field.parse::<u32>().ok()) {
                Some(id) => id,
                None => continue,
            };
            map.entry(id).or_insert_with(|| name.to_string());
        }
    }

    map
}
