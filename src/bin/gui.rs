#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui::{self, Color32, RichText};
use ratio_mini::{Args, Event, inspect, run_session};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
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
}

impl Session {
    fn request_stop(&mut self) {
        if self.running {
            self.stop.store(true, Ordering::SeqCst);
            self.status = "Arrêt en cours…".into();
        }
    }

    fn poll(&mut self) {
        while self.running {
            match self.receiver.try_recv() {
                Ok(Message::Update(Event::Log(line))) => {
                    if line.starts_with("started accepté") && !self.stop.load(Ordering::SeqCst) {
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
    rate: u64,
    target: u64,
    variation: u8,
    ramp: u16,
    sessions: Vec<Session>,
    error: Option<String>,
    closing: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            file: None,
            rate: 128,
            target: 1024,
            variation: 25,
            ramp: 30,
            sessions: Vec::new(),
            error: None,
            closing: false,
        }
    }
}

impl App {
    fn add(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        let id = match inspect(&file) {
            Ok(id) => id,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        if self.sessions.iter().any(|s| s.id == id && s.running) {
            self.error = Some("Ce torrent a déjà une session active.".into());
            return;
        }
        let settings = Args {
            torrent: file.clone(),
            upload_mib: self.target.clamp(1, 1_000_000),
            rate_kib: self.rate.clamp(1, 1_000_000),
            variation_percent: self.variation.min(100),
            ramp_seconds: self.ramp.min(3600),
            send: true,
            port: 6881,
        };
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
            return;
        }
        self.sessions.push(Session {
            id,
            name: file
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
        });
        self.file = None;
        self.error = None;
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        ui.heading("Ajouter un torrent");
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
            self.file = Some(path);
            self.error = None;
        }
        if let Some(file) = &self.file {
            ui.label(
                RichText::new(file.file_name().unwrap_or_default().to_string_lossy()).strong(),
            );
        } else {
            ui.weak("Aucun fichier sélectionné");
        }
        ui.add_space(18.0);
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
        ui.label("Volume maximal de la session");
        ui.add(
            egui::DragValue::new(&mut self.target)
                .range(1..=1_000_000)
                .speed(64)
                .suffix(" Mio"),
        );
        ui.add_space(12.0);
        ui.label("Variations du débit");
        ui.add(egui::Slider::new(&mut self.variation, 0..=100).suffix(" %"));
        ui.label("Démarrage progressif");
        ui.add(
            egui::DragValue::new(&mut self.ramp)
                .range(0..=3600)
                .suffix(" s"),
        );
        ui.add_space(18.0);
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
        ui.add_space(8.0);
        ui.weak(
            "Chaque session garde ses propres réglages. Le bouton démarre les annonces réseau.",
        );
        if let Some(error) = &self.error {
            ui.add_space(12.0);
            ui.colored_label(Color32::from_rgb(255, 145, 145), error);
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
        egui::ScrollArea::vertical().id_salt("sessions").show(ui, |ui| {
            for (index, session) in self.sessions.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 2.0).max(0.0));
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&session.name).strong());
                            let color = if session.failed { Color32::from_rgb(255, 145, 145) } else if session.running { Color32::from_rgb(100, 215, 175) } else { Color32::LIGHT_GRAY };
                            ui.colored_label(color, &session.status);
                        });
                        ui.add_space(6.0);
                        let target = session.settings.upload_mib as f64 * 1_048_576.0;
                        ui.add(egui::ProgressBar::new((session.simulated as f64 / target).clamp(0.0, 1.0) as f32).text(format!("{:.1} / {} Mio simulés", session.simulated as f64 / 1_048_576.0, session.settings.upload_mib)));
                        ui.label(format!("{:.0} Kio/s actuellement  ·  moyenne visée {} Kio/s  ·  variation ±{} %", session.speed, session.settings.rate_kib, session.settings.variation_percent));
                        ui.weak(format!("Dernière déclaration acceptée : {:.1} Mio", session.announced as f64 / 1_048_576.0));
                        ui.horizontal(|ui| {
                            if session.running {
                                if ui.add_enabled(!session.stop.load(Ordering::SeqCst), egui::Button::new("Arrêter")).clicked() { session.request_stop(); }
                            } else if ui.button("Retirer").clicked() { remove = Some(index); }
                            ui.collapsing("Journal", |ui| { for line in &session.logs { ui.small(line); } });
                        });
                    });
                    ui.add_space(10.0);
                });
            }
        });
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
            ui.horizontal(|ui| { ui.heading(RichText::new("Ratio Mini").size(27.0)); ui.label("0.2"); });
            ui.label("Upload simulé — aucun transfert des fichiers du torrent.");
            if self.closing { ui.colored_label(Color32::YELLOW, "Arrêt des sessions avant fermeture… Une requête peut prendre jusqu’à 20 secondes."); }
            ui.add_space(12.0);
        });
        egui::SidePanel::left("add")
            .exact_width(300.0)
            .resizable(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(14.0);
                    self.form(ui);
                });
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(14.0);
            self.list(ui);
        });
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

impl Drop for App {
    fn drop(&mut self) {
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
            Ok(Box::<App>::default())
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_session(name: &str) -> (Session, mpsc::Sender<Message>) {
        let (tx, receiver) = mpsc::channel();
        (
            Session {
                id: name.into(),
                name: name.into(),
                settings: Args {
                    torrent: name.into(),
                    upload_mib: 1024,
                    rate_kib: 128,
                    variation_percent: 25,
                    ramp_seconds: 30,
                    send: true,
                    port: 6881,
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
