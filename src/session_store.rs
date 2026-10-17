//! Portable session list. Only paths and settings are saved; nothing starts on restore.
use crate::{Args, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormSettings {
    pub rate: u64,
    pub target: u64,
    pub variation: u8,
    pub ramp: u16,
    pub pause_without_leechers: bool,
    #[serde(default)]
    pub client_profile: crate::profiles::ClientProfile,
    #[serde(default)]
    pub handshake: bool,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    6881
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub version: u32,
    pub defaults: FormSettings,
    pub torrents: Vec<Args>,
}

impl Document {
    fn validate(&self) -> Result<()> {
        fn valid(rate: u64, target: u64, variation: u8, ramp: u16) -> bool {
            (1..=1_000_000).contains(&rate)
                && (1..=1_000_000).contains(&target)
                && variation <= 100
                && ramp <= 3600
        }
        if self.version != 1
            || self.torrents.len() > 256
            || !valid(
                self.defaults.rate,
                self.defaults.target,
                self.defaults.variation,
                self.defaults.ramp,
            )
            || self.defaults.port == 0
            || self.torrents.iter().any(|a| {
                !valid(
                    a.rate_kib,
                    a.upload_mib,
                    a.variation_percent,
                    a.ramp_seconds,
                ) || a.port == 0
            })
        {
            return Err("Liste de sessions incompatible ou paramètres invalides.".into());
        }
        Ok(())
    }
}

pub fn load(path: &Path) -> Result<Option<Document>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Impossible de lire la liste de sessions.".into()),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Lecture des sessions impossible")?;
    if bytes.len() > 1024 * 1024 {
        return Err("Liste de sessions trop volumineuse.".into());
    }
    let document: Document = serde_json::from_slice(&bytes)
        .map_err(|_| "Fichier de sessions JSON invalide ; l'original est conservé.")?;
    document.validate()?;
    Ok(Some(document))
}

pub fn save(path: &Path, document: &Document) -> Result<()> {
    document.validate()?;
    let bytes =
        serde_json::to_vec_pretty(document).map_err(|_| "Encodage des sessions impossible")?;
    if bytes.len() > 1024 * 1024 {
        return Err("Liste de sessions trop volumineuse.".into());
    }
    let parent = path.parent().ok_or("Dossier de sauvegarde absent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "Dossier de sauvegarde non accessible en écriture")?;
    temp.write_all(&bytes)
        .map_err(|_| "Écriture des sessions impossible")?;
    temp.as_file()
        .sync_all()
        .map_err(|_| "Synchronisation des sessions impossible")?;
    temp.persist(path)
        .map_err(|_| "Remplacement du fichier de sessions impossible")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> Document {
        Document {
            version: 1,
            defaults: FormSettings {
                rate: 128,
                target: 1024,
                variation: 25,
                ramp: 30,
                pause_without_leechers: true,
                client_profile: crate::profiles::ClientProfile::Native,
                handshake: false,
                port: 6881,
            },
            torrents: vec![Args {
                torrent: "chemin avec espaces/été.torrent".into(),
                upload_mib: 2048,
                rate_kib: 512,
                variation_percent: 15,
                ramp_seconds: 60,
                send: true,
                port: 6881,
                ignore_swarm: false,
                client_profile: crate::profiles::ClientProfile::Native,
                handshake: false,
            }],
        }
    }
    #[test]
    fn round_trip_and_atomic_replacement_preserve_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        assert!(load(&path).unwrap().is_none());
        let mut doc = document();
        save(&path, &doc).unwrap();
        assert_eq!(load(&path).unwrap(), Some(doc.clone()));
        doc.torrents[0].rate_kib = 256;
        save(&path, &doc).unwrap();
        assert_eq!(load(&path).unwrap(), Some(doc));
    }
    #[test]
    fn invalid_update_preserves_previous_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let mut doc = document();
        save(&path, &doc).unwrap();
        let original = std::fs::read(&path).unwrap();
        doc.torrents[0].rate_kib = 0;
        assert!(save(&path, &doc).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::write(&path, b"{broken").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
    }

    #[test]
    fn old_session_files_default_to_native_profile() {
        let mut value = serde_json::to_value(document()).unwrap();
        value["defaults"]
            .as_object_mut()
            .unwrap()
            .remove("client_profile");
        value["defaults"]
            .as_object_mut()
            .unwrap()
            .remove("handshake");
        value["defaults"].as_object_mut().unwrap().remove("port");
        value["torrents"][0]
            .as_object_mut()
            .unwrap()
            .remove("client_profile");
        value["torrents"][0]
            .as_object_mut()
            .unwrap()
            .remove("handshake");
        let loaded: Document = serde_json::from_value(value).unwrap();
        assert_eq!(
            loaded.defaults.client_profile,
            crate::profiles::ClientProfile::Native
        );
        assert!(!loaded.defaults.handshake && !loaded.torrents[0].handshake);
        assert_eq!(loaded.defaults.port, 6881);
        assert_eq!(
            loaded.torrents[0].client_profile,
            crate::profiles::ClientProfile::Native
        );
    }
}
