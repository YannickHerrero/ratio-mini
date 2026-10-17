#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui::{self, Color32, RichText};
use ratio_mini::{
    Args, Event, TorrentMetadata, inspect_details, run_session,
    session_store::{self, Document, FormSettings},
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

enum Message {
    Update(Event),
    Finished(Result<(), String>),
}

struct Session {
    id: String,
    name: String,
    settings: Args,
    stop: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Message>,
    running: bool,
    status: String,
    simulated: u64,
    announced: u64,
    speed: f64,
    logs: VecDeque<String>,
    failed: bool,
    paused: bool,
    seeders: Option<u64>,
    leechers: Option<u64>,
    listener_port: Option<u16>,
    handshakes: u64,
    target_bytes: Option<u64>,
    torrent_size: Option<u64>,
}

impl Session {
    fn idle(settings: Args) -> Self {
        let (_tx, receiver) = mpsc::channel();
        Self {
            id: String::new(),
            name: settings
                .torrent
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            settings,
            stop: Arc::new(AtomicBool::new(false)),
            receiver,
            running: false,
            status: "Prêt à démarrer".into(),
            simulated: 0,
            announced: 0,
            speed: 0.0,
            logs: VecDeque::new(),
            failed: false,
            paused: false,
            seeders: None,
            leechers: None,
            listener_port: None,
            handshakes: 0,
            target_bytes: None,
            torrent_size: None,
        }
    }
    fn request_stop(&mut self) {
        if self.running {
            self.stop.store(true, Ordering::SeqCst);
            self.status = "Arrêt en cours…".into();
        }
    }

    fn poll(&mut self) {
        while self.running {
            match self.receiver.try_recv() {
                Ok(Message::Update(Event::Metadata {
                    torrent_size,
                    target_bytes,
                })) => {
                    self.torrent_size = Some(torrent_size);
                    self.target_bytes = Some(target_bytes);
                }
                Ok(Message::Update(Event::Listener { port, answered })) => {
                    self.listener_port = Some(port);
                    self.handshakes = answered;
                }
                Ok(Message::Update(Event::Retrying { seconds, attempt })) => {
                    if !self.stop.load(Ordering::SeqCst) {
                        self.status = format!("Nouvelle tentative {attempt}/3 dans {seconds} s");
                    }
                    self.speed = 0.0;
                }
                Ok(Message::Update(Event::Log(line))) => {
                    if line.starts_with("started accepté")
                        && !self.paused
                        && !self.stop.load(Ordering::SeqCst)
                    {
                        self.status = "En cours".into();
                    }
                    if self.logs.len() >= 60 {
                        self.logs.pop_front();
                    }
                    self.logs.push_back(line);
                }
                Ok(Message::Update(Event::Progress {
                    simulated,
                    announced,
                    rate_kib,
                })) => {
                    self.simulated = simulated;
                    self.announced = announced;
                    self.speed = rate_kib;
                }
                Ok(Message::Update(Event::Swarm {
                    seeders,
                    leechers,
                    paused,
                })) => {
                    self.seeders = seeders;
                    self.leechers = leechers;
                    self.paused = paused;
                    if !self.stop.load(Ordering::SeqCst) {
                        self.status = if paused {
                            "Pause : aucun leecher"
                        } else {
                            "En cours"
                        }
                        .into();
                    }
                }
                Ok(Message::Finished(result)) => {
                    self.running = false;
                    self.speed = 0.0;
                    self.failed = result.is_err();
                    self.status = match result {
                        Ok(()) if self.stop.load(Ordering::SeqCst) => "Arrêté".into(),
                        Ok(()) => "Terminé".into(),
                        Err(error) => {
                            self.logs.push_back(error);
                            "Erreur".into()
                        }
                    };
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.running = false;
                    self.failed = true;
                    self.speed = 0.0;
                    self.status = "Session interrompue".into();
                    break;
                }
            }
        }
    }
}

struct App {
    file: Option<PathBuf>,
    file_details: Option<TorrentMetadata>,
    rate: u64,
    target: u64,
    percentage_mode: bool,
    upload_percent: u32,
    variation: u8,
    ramp: u16,
    pause_without_leechers: bool,
    client_profile: ratio_mini::profiles::ClientProfile,
    handshake: bool,
    port: u16,
    sessions: Vec<Session>,
    error: Option<String>,
    closing: bool,
    storage: Option<PathBuf>,
    saved: Option<Document>,
    next_save: Instant,
}

impl Default for App {
    fn default() -> Self {
        let defaults = FormSettings::default();
        Self {
            file: None,
            file_details: None,
            rate: defaults.rate,
            target: defaults.target,
            percentage_mode: defaults.upload_percent.is_some(),
            upload_percent: defaults.upload_percent.unwrap_or(300),
            variation: defaults.variation,
            ramp: defaults.ramp,
            pause_without_leechers: defaults.pause_without_leechers,
            client_profile: defaults.client_profile,
            handshake: defaults.handshake,
            port: defaults.port,
            sessions: Vec::new(),
            error: None,
            closing: false,
            storage: None,
            saved: None,
            next_save: Instant::now(),
        }
    }
}

impl App {
    fn reset_form(&mut self) {
        let defaults = FormSettings::default();
        self.rate = defaults.rate;
        self.target = defaults.target;
        self.percentage_mode = defaults.upload_percent.is_some();
        self.upload_percent = defaults.upload_percent.unwrap_or(300);
        self.variation = defaults.variation;
        self.ramp = defaults.ramp;
        self.pause_without_leechers = defaults.pause_without_leechers;
        self.client_profile = defaults.client_profile;
        self.handshake = defaults.handshake;
        self.port = defaults.port;
    }
    fn add(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        let settings = self.settings_for(file);
        if self.launch(settings, None) {
            self.file = None;
            self.file_details = None;
        }
    }

    fn settings_for(&self, file: PathBuf) -> Args {
        Args {
            torrent: file,
            upload_mib: (!self.percentage_mode).then_some(self.target.clamp(1, 1_000_000)),
            upload_percent: self
                .percentage_mode
                .then_some(self.upload_percent.clamp(1, 100_000)),
            rate_kib: self.rate.clamp(1, 1_000_000),
            variation_percent: self.variation.min(100),
            ramp_seconds: self.ramp.min(3600),
            send: true,
            port: self.port,
            ignore_swarm: !self.pause_without_leechers,
            client_profile: self.client_profile,
            handshake: self.handshake,
        }
    }

    fn launch(&mut self, mut settings: Args, replace: Option<usize>) -> bool {
        settings.send = true;
        if replace.is_none() && self.sessions.len() >= 256 {
            self.error = Some("La liste est limitée à 256 torrents.".into());
            return false;
        }
        let details = match inspect_details(&settings.torrent) {
            Ok(details) => details,
            Err(error) => {
                self.error = Some(error);
                return false;
            }
        };
        let target_bytes = match settings.target_bytes(details.size) {
            Ok(target) => target,
            Err(error) => {
                self.error = Some(error);
                return false;
            }
        };
        let id = details.id;
        if self.sessions.iter().any(|s| s.id == id && s.running) {
            self.error = Some("Ce torrent a déjà une session active.".into());
            return false;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, receiver) = mpsc::channel();
        let worker_args = settings.clone();
        let worker_stop = stop.clone();
        if thread::Builder::new()
            .name("torrent-session".into())
            .spawn(move || {
                let result = run_session(worker_args, worker_stop, |event| {
                    let _ = tx.send(Message::Update(event));
                });
                let _ = tx.send(Message::Finished(result));
            })
            .is_err()
        {
            self.error = Some("Impossible de démarrer une session supplémentaire.".into());
            return false;
        }
        let session = Session {
            id,
            name: settings
                .torrent
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            settings,
            stop,
            receiver,
            running: true,
            status: "Connexion…".into(),
            simulated: 0,
            announced: 0,
            speed: 0.0,
            logs: VecDeque::new(),
            failed: false,
            paused: false,
            seeders: None,
            leechers: None,
            listener_port: None,
            handshakes: 0,
            target_bytes: Some(target_bytes),
            torrent_size: Some(details.size),
        };
        if let Some(index) = replace {
            self.sessions[index] = session;
        } else {
            self.sessions.push(session);
        }
        self.error = None;
        true
    }

    fn document(&self) -> Document {
        Document {
            version: 1,
            defaults: FormSettings {
                rate: self.rate,
                target: self.target,
                upload_percent: self.percentage_mode.then_some(self.upload_percent),
                variation: self.variation,
                ramp: self.ramp,
                pause_without_leechers: self.pause_without_leechers,
                client_profile: self.client_profile,
                handshake: self.handshake,
                port: self.port,
            },
            torrents: self.sessions.iter().map(|s| s.settings.clone()).collect(),
        }
    }

    fn apply_document(&mut self, doc: Document) {
        self.rate = doc.defaults.rate;
        self.target = doc.defaults.target;
        self.percentage_mode = doc.defaults.upload_percent.is_some();
        self.upload_percent = doc.defaults.upload_percent.unwrap_or(300);
        self.variation = doc.defaults.variation;
        self.ramp = doc.defaults.ramp;
        self.pause_without_leechers = doc.defaults.pause_without_leechers;
        self.client_profile = doc.defaults.client_profile;
        self.handshake = doc.defaults.handshake;
        self.port = doc.defaults.port;
        self.sessions = doc.torrents.iter().cloned().map(Session::idle).collect();
        self.saved = Some(doc);
        self.error = None;
    }

    fn restore(path: PathBuf) -> Self {
        let mut app = Self::default();
        match session_store::load(&path) {
            Ok(Some(doc)) => {
                app.apply_document(doc);
                app.storage = Some(path);
            }
            Ok(None) => app.storage = Some(path),
            Err(error) => {
                app.error = Some(format!(
                    "{error} Sauvegarde automatique désactivée pour préserver ce fichier."
                ));
            }
        }
        app
    }

    fn persist(&mut self) {
        let Some(path) = self.storage.as_ref() else {
            return;
        };
        let doc = self.document();
        if self.saved.as_ref() == Some(&doc) {
            return;
        }
        match session_store::save(path, &doc) {
            Ok(()) => self.saved = Some(doc),
            Err(error) => self.error = Some(error),
        }
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        ui.heading("Ajouter un torrent");
        if ui
            .small_button("Valeurs par défaut")
            .on_hover_text(
                "Rétablir les réglages d'ajout ; les sessions existantes gardent leurs réglages.",
            )
            .clicked()
        {
            self.reset_form();
        }
        ui.add_space(8.0);
        ui.label("Choisis un fichier, puis les réglages de sa session.");
        ui.add_space(14.0);
        if ui
            .add_sized(
                [ui.available_width(), 36.0],
                egui::Button::new("Choisir un fichier .torrent…"),
            )
            .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Torrent", &["torrent"])
                .pick_file()
        {
            match inspect_details(&path) {
                Ok(details) => {
                    self.file = Some(path);
                    self.file_details = Some(details);
                    self.error = None;
                }
                Err(error) => {
                    self.file = None;
                    self.file_details = None;
                    self.error = Some(error);
                }
            }
        }
        if let Some(file) = &self.file {
            ui.label(
                RichText::new(file.file_name().unwrap_or_default().to_string_lossy()).strong(),
            );
        } else {
            ui.weak("Aucun fichier sélectionné");
        }
        ui.add_space(18.0);
        ui.label("Profil du client");
        egui::ComboBox::from_id_salt("profile")
            .selected_text(self.client_profile.label())
            .show_ui(ui, |ui| {
                for profile in ratio_mini::profiles::ClientProfile::ALL {
                    ui.selectable_value(&mut self.client_profile, profile, profile.label());
                }
            });
        if self.client_profile != ratio_mini::profiles::ClientProfile::Native {
            ui.weak("Profil : connexion directe HTTP(S).");
        }
        ui.add_space(8.0);
        ui.label("Vitesse moyenne visée");
        ui.add(
            egui::DragValue::new(&mut self.rate)
                .range(1..=1_000_000)
                .speed(8)
                .suffix(" Kio/s"),
        );
        ui.weak(format!(
            "≈ {:.2} Gio par heure, par torrent",
            self.rate as f64 * 3600.0 / 1_048_576.0
        ));
        ui.add_space(12.0);
        ui.label("Plafond d’upload de la session");
        egui::ComboBox::from_id_salt("limit-mode")
            .selected_text(if self.percentage_mode {
                "Pourcentage du torrent"
            } else {
                "Volume fixe"
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.percentage_mode, true, "Pourcentage du torrent");
                ui.selectable_value(&mut self.percentage_mode, false, "Volume fixe");
            });
        if self.percentage_mode {
            ui.add(
                egui::DragValue::new(&mut self.upload_percent)
                    .range(1..=100_000)
                    .speed(25)
                    .suffix(" %"),
            );
            ui.weak(format!(
                "Soit {:.2} × la taille du torrent",
                f64::from(self.upload_percent) / 100.0
            ));
        } else {
            ui.add(
                egui::DragValue::new(&mut self.target)
                    .range(1..=1_000_000)
                    .speed(64)
                    .suffix(" Mio"),
            );
        }
        if let (Some(file), Some(details)) = (&self.file, &self.file_details) {
            match self.settings_for(file.clone()).target_bytes(details.size) {
                Ok(target) => {
                    ui.weak(format!(
                        "Taille : {:.2} Gio · plafond : {:.2} Gio",
                        details.size as f64 / 1_073_741_824.0,
                        target as f64 / 1_073_741_824.0
                    ));
                }
                Err(error) => {
                    ui.colored_label(Color32::LIGHT_RED, error);
                }
            }
        }
        ui.add_space(12.0);
        ui.label("Variations du débit");
        ui.add(egui::Slider::new(&mut self.variation, 0..=100).suffix(" %"));
        ui.label("Démarrage progressif");
        ui.add(
            egui::DragValue::new(&mut self.ramp)
                .range(0..=3600)
                .suffix(" s"),
        );
        ui.checkbox(&mut self.pause_without_leechers, "Pause si aucun leecher");
        ui.checkbox(&mut self.handshake, "Répondre au handshake TCP");
        ui.horizontal(|ui| {
            ui.label("Port partagé");
            ui.add(egui::DragValue::new(&mut self.port).range(1..=65535));
        });
        ui.add_space(18.0);
        ui.add_space(8.0);
        ui.weak(
            "Chaque session garde ses propres réglages. Le bouton démarre les annonces réseau.",
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Enregistrer…").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Sessions JSON", &["json"])
                    .set_file_name("ratio-mini-sessions.json")
                    .save_file()
            {
                match session_store::save(&path, &self.document()) {
                    Ok(()) => {
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            if ui
                .add_enabled(
                    !self.sessions.iter().any(|s| s.running),
                    egui::Button::new("Charger…"),
                )
                .on_hover_text("Disponible lorsque toutes les sessions sont arrêtées")
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Sessions JSON", &["json"])
                    .pick_file()
            {
                match session_store::load(&path) {
                    Ok(Some(doc)) => {
                        self.apply_document(doc);
                        self.saved = None;
                    }
                    Ok(None) => self.error = Some("Fichier introuvable.".into()),
                    Err(error) => self.error = Some(error),
                }
            }
        });
        ui.weak("Liste et réglages conservés à côté du programme. Les sessions restaurées restent arrêtées.");
        if let Some(error) = &self.error {
            ui.add_space(12.0);
            ui.colored_label(Color32::from_rgb(255, 145, 145), error);
        }
    }

    fn add_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(
                self.file.is_some() && !self.closing,
                egui::Button::new(RichText::new("Ajouter et démarrer").strong())
                    .min_size(egui::vec2(ui.available_width(), 40.0)),
            )
            .clicked()
        {
            self.add();
        }
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        let active = self.sessions.iter().filter(|s| s.running).count();
        ui.horizontal(|ui| {
            ui.heading("Mes sessions");
            ui.label(format!("{active} active(s)"));
            if ui
                .add_enabled(active > 0, egui::Button::new("Tout arrêter"))
                .clicked()
            {
                for session in &mut self.sessions {
                    session.request_stop();
                }
            }
            if ui
                .button("Nettoyer la liste")
                .on_hover_text("Retirer les sessions terminées ou en erreur")
                .clicked()
            {
                self.sessions.retain(|s| s.running);
            }
        });
        ui.add_space(6.0);
        let speed = self
            .sessions
            .iter()
            .fold(0.0_f64, |total, s| total + s.speed);
        let announced: u64 = self.sessions.iter().map(|s| s.announced).sum();
        ui.weak(format!(
            "Débit simulé cumulé : {speed:.0} Kio/s  •  Déclaré : {:.1} Mio",
            announced as f64 / 1_048_576.0
        ));
        ui.add_space(12.0);
        if self.sessions.is_empty() {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.heading("La liste est vide");
                ui.label("Ajoute ton premier torrent depuis le panneau de gauche.");
            });
        }
        let mut remove = None;
        let mut restart = None;
        egui::ScrollArea::vertical().id_salt("sessions").show(ui, |ui| {
            for (index, session) in self.sessions.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    egui::Frame::group(ui.style()).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 2.0).max(0.0));
                        let id = ui.make_persistent_id(("session-details", &session.settings.torrent));
                        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
                        let header = ui.horizontal(|ui| {
                            state.show_toggle_button(ui, egui::collapsing_header::paint_default_icon);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(format!("{:.0} Kio/s", session.speed));
                                let color = if session.failed { Color32::from_rgb(255, 145, 145) } else if session.running { Color32::from_rgb(100, 215, 175) } else { Color32::LIGHT_GRAY };
                                ui.add(egui::Label::new(RichText::new(&session.status).color(color)).truncate());
                                ui.add_sized([ui.available_width(), ui.spacing().interact_size.y], egui::Label::new(RichText::new(&session.name).strong()).halign(egui::Align::Min).truncate())
                                    .on_hover_text(&session.name);
                            });
                        });
                        if header.response.interact(egui::Sense::click()).on_hover_text("Afficher ou replier les détails").clicked() {
                            state.toggle(ui);
                        }
                        state.show_body_unindented(ui, |ui| {
                        ui.weak(session.settings.client_profile.label());
                        ui.add_space(6.0);
                        let limit_label = match (session.settings.upload_mib, session.settings.upload_percent) {
                            (Some(mib), _) => format!("Plafond fixe : {mib} Mio"),
                            (None, percent) => format!("Plafond : {} % de la taille du torrent", percent.unwrap_or(300)),
                        };
                        ui.weak(limit_label);
                        let (fraction, progress_text) = match session.target_bytes {
                            Some(target) => ((session.simulated as f64 / target as f64).clamp(0.0, 1.0) as f32, format!("{:.2} / {:.2} Mio simulés", session.simulated as f64 / 1_048_576.0, target as f64 / 1_048_576.0)),
                            None => (0.0, "Plafond calculé au démarrage".into()),
                        };
                        ui.add(egui::ProgressBar::new(fraction).text(progress_text));
                        if let Some(size) = session.torrent_size { ui.weak(format!("Taille du torrent : {:.2} Gio", size as f64 / 1_073_741_824.0)); }
                        ui.label(format!("{:.0} Kio/s actuellement  ·  moyenne visée {} Kio/s  ·  variation ±{} %", session.speed, session.settings.rate_kib, session.settings.variation_percent));
                        ui.weak(format!("Dernière déclaration acceptée : {:.1} Mio", session.announced as f64 / 1_048_576.0));
                        let count = |value: Option<u64>| value.map_or_else(|| "?".into(), |n| n.to_string());
                        ui.weak(format!("Derniers chiffres — seeders : {}  ·  leechers : {}", count(session.seeders), count(session.leechers)));
                        if let Some(port) = session.listener_port {
                            ui.weak(format!("Handshake TCP : port {port}  ·  {} réponse(s){}", session.handshakes, if session.running { "" } else { " · écoute arrêtée" }));
                        }
                        ui.horizontal(|ui| {
                            if session.running {
                                if ui.add_enabled(!session.stop.load(Ordering::SeqCst), egui::Button::new("Arrêter")).clicked() { session.request_stop(); }
                            } else {
                                if ui.add_enabled(!self.closing, egui::Button::new("Démarrer")).clicked() { restart = Some(index); }
                                if ui.button("Retirer").clicked() { remove = Some(index); }
                            }
                            ui.collapsing("Journal", |ui| { for line in &session.logs { ui.small(line); } });
                        });
                        });
                    });
                    ui.add_space(4.0);
                });
            }
        });
        if let Some(index) = restart {
            let settings = self.sessions[index].settings.clone();
            self.launch(settings, Some(index));
        }
        if let Some(index) = remove {
            self.sessions.remove(index);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for session in &mut self.sessions {
            session.poll();
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.sessions.iter().any(|s| s.running)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
            for session in &mut self.sessions {
                session.request_stop();
            }
        }
        if self.closing && self.sessions.iter().all(|s| !s.running) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::TopBottomPanel::top("header").exact_height(if self.closing { 120.0 } else { 90.0 }).show(ctx, |ui| {
            ui.add_space(12.0);
            ui.horizontal(|ui| { ui.heading(RichText::new("Ratio Mini").size(27.0)); ui.label(env!("CARGO_PKG_VERSION")); });
            ui.label("Upload simulé — aucun transfert des fichiers du torrent.");
            if self.closing { ui.colored_label(Color32::YELLOW, "Arrêt des sessions avant fermeture… Une requête peut prendre jusqu’à 20 secondes."); }
            ui.add_space(12.0);
        });
        egui::SidePanel::left("add")
            .exact_width(300.0)
            .resizable(false)
            .show(ctx, |ui| {
                let settings_height = (ui.available_height() - 60.0).max(0.0);
                egui::ScrollArea::vertical()
                    .max_height(settings_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(14.0);
                        self.form(ui);
                    });
                ui.separator();
                self.add_button(ui);
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(14.0);
            self.list(ui);
        });
        if Instant::now() >= self.next_save
            || self.closing
            || ctx.input(|i| i.viewport().close_requested())
        {
            self.persist();
            self.next_save = Instant::now() + Duration::from_secs(2);
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.persist();
        for session in &self.sessions {
            session.stop.store(true, Ordering::SeqCst);
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Ratio Mini",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let mut style = (*cc.egui_ctx.style()).clone();
            style.spacing.item_spacing = egui::vec2(9.0, 8.0);
            style.visuals.selection.bg_fill = Color32::from_rgb(36, 110, 95);
            cc.egui_ctx.set_style(style);
            let app = match std::env::current_exe() {
                Ok(exe) => App::restore(exe.with_file_name("ratio-mini-sessions.json")),
                Err(_) => App::default(),
            };
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_changes_new_session_defaults_without_touching_existing_sessions_or_selection() {
        let mut app = App::default();
        let (session, _tx) = fake_session("existing.torrent");
        let settings = session.settings.clone();
        app.sessions.push(session);
        app.file = Some("selected.torrent".into());
        app.rate = 999;
        app.percentage_mode = false;
        app.upload_percent = 10;
        app.reset_form();
        let new_settings = app.settings_for(app.file.clone().unwrap());
        assert_eq!(new_settings.upload_mib, None);
        assert_eq!(new_settings.upload_percent, Some(300));
        assert_eq!(new_settings.rate_kib, 128);
        assert_eq!(
            new_settings.client_profile,
            ratio_mini::profiles::ClientProfile::Qbittorrent524
        );
        assert!(new_settings.handshake && !new_settings.ignore_swarm);
        assert_eq!(app.sessions[0].settings, settings);
        assert_eq!(
            app.file.as_deref(),
            Some(std::path::Path::new("selected.torrent"))
        );
    }
    #[test]
    fn restored_list_never_starts_sessions_or_reads_torrent_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let (session, _tx) = fake_session("missing-file.torrent");
        let mut original = App::default();
        original.sessions.push(session);
        session_store::save(&path, &original.document()).unwrap();
        let restored = App::restore(path);
        assert_eq!(restored.sessions.len(), 1);
        assert!(!restored.sessions[0].running);
        assert_eq!(restored.sessions[0].announced, 0);
        assert_eq!(restored.sessions[0].status, "Prêt à démarrer");
    }

    #[test]
    fn corrupt_autosave_is_not_overwritten_by_app_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        std::fs::write(&path, "{invalid").unwrap();
        let app = App::restore(path.clone());
        assert!(app.error.is_some());
        drop(app);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{invalid");
    }

    #[test]
    fn paused_state_survives_started_log_and_stop_takes_priority() {
        let (mut session, tx) = fake_session("empty-swarm");
        tx.send(Message::Update(Event::Swarm {
            seeders: Some(5),
            leechers: Some(0),
            paused: true,
        }))
        .unwrap();
        tx.send(Message::Update(Event::Log("started accepté".into())))
            .unwrap();
        session.poll();
        assert_eq!(session.status, "Pause : aucun leecher");
        session.request_stop();
        tx.send(Message::Update(Event::Swarm {
            seeders: Some(5),
            leechers: Some(2),
            paused: false,
        }))
        .unwrap();
        session.poll();
        assert_eq!(session.status, "Arrêt en cours…");
    }

    fn fake_session(name: &str) -> (Session, mpsc::Sender<Message>) {
        let (tx, receiver) = mpsc::channel();
        (
            Session {
                id: name.into(),
                name: name.into(),
                settings: Args {
                    torrent: name.into(),
                    upload_mib: Some(1024),
                    upload_percent: None,
                    rate_kib: 128,
                    variation_percent: 25,
                    ramp_seconds: 30,
                    send: true,
                    port: 6881,
                    ignore_swarm: false,
                    client_profile: ratio_mini::profiles::ClientProfile::Native,
                    handshake: false,
                },
                stop: Arc::new(AtomicBool::new(false)),
                receiver,
                running: true,
                status: "Connexion…".into(),
                simulated: 0,
                announced: 0,
                speed: 0.0,
                logs: VecDeque::new(),
                failed: false,
                paused: false,
                seeders: None,
                leechers: None,
                listener_port: None,
                handshakes: 0,
                target_bytes: None,
                torrent_size: None,
            },
            tx,
        )
    }

    #[test]
    fn sessions_receive_independent_progress_and_stop_requests() {
        let (mut first, first_tx) = fake_session("one");
        let (mut second, second_tx) = fake_session("two");
        first_tx
            .send(Message::Update(Event::Progress {
                simulated: 2048,
                announced: 1024,
                rate_kib: 42.0,
            }))
            .unwrap();
        second_tx
            .send(Message::Update(Event::Progress {
                simulated: 9999,
                announced: 8888,
                rate_kib: 120.0,
            }))
            .unwrap();
        first.poll();
        second.poll();
        assert_eq!((first.simulated, first.announced), (2048, 1024));
        assert_eq!((second.simulated, second.announced), (9999, 8888));
        first.request_stop();
        assert!(first.stop.load(Ordering::SeqCst));
        assert!(!second.stop.load(Ordering::SeqCst));
        first_tx.send(Message::Finished(Ok(()))).unwrap();
        first.poll();
        assert_eq!(first.status, "Arrêté");
        assert!(!first.running);
        assert!(second.running);
    }

    #[test]
    fn error_is_visible_and_releases_session() {
        let (mut session, tx) = fake_session("bad");
        tx.send(Message::Finished(Err("Tracker inaccessible".into())))
            .unwrap();
        session.poll();
        assert!(!session.running && session.failed);
        assert_eq!(session.logs.back().unwrap(), "Tracker inaccessible");
    }

    #[test]
    fn both_panels_render_with_multiple_sessions() {
        let ctx = egui::Context::default();
        let (first, _a) = fake_session("Premier torrent.torrent");
        let (second, _b) = fake_session("Deuxième torrent.torrent");
        let mut app = App::default();
        app.sessions.extend([first, second]);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 720.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::SidePanel::left("test-add")
                    .exact_width(300.0)
                    .show(ctx, |ui| app.form(ui));
                egui::CentralPanel::default().show(ctx, |ui| app.list(ui));
            },
        );
        assert!(!output.shapes.is_empty());
        assert_eq!(app.sessions.len(), 2);
    }
}
