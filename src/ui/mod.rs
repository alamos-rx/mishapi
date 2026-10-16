//! Desktop UI (egui). `App` holds the state; each file adds part of its behavior.

mod agent_panel;
mod auth_editor;
mod bars;
mod dialogs;
mod editor;
mod repos;
mod response;
mod run_window;
mod send;
mod session;
mod sidebar;
mod style;
pub mod theme;
mod util;
pub mod widgets;

use auth_editor::*;
use style::*;
use util::*;
use widgets::{Lang, Row};

use std::{
    collections::HashSet,
    path::Path,
    time::Instant,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc::{Receiver, TryRecvError, channel},
    },
};

use eframe::egui::{self, Align, Button, Color32, Id, Key, Layout, Modifiers, RichText, ScrollArea, TextEdit, TextStyle};
use serde::de::DeserializeOwned;

use crate::{
    i18n::{self, t},
    tr,
    domain::{
        actions::{self, Action, Effects, Launch, Ws},
        model::{self, AgentCfg, Collection, Environments, RepoCfg, Request, Response, Settings, Source, Stage},
        overlay,
        vars::{self, Vars, resolve},
    },
    engine::{
        runner::{self, SendDone},
        script::{ReqParts, Scopes, TestResult},
    },
    import::{self, Imported},
    infra::{
        agent, auth, cookies, http, repo,
        storage::{self, AGENTS, COLLECTIONS, ENVIRONMENTS, SETTINGS},
    },
};

const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const HISTORY_MAX: usize = 100;
const VIEW_LIMIT: usize = 2_000_000; // ponytail: the egui viewer gets slow with huge bodies; they are truncated
const REPO_CACHE: &str = "repos"; // sparse clones of the repos, next to the app's JSON files
const AGENT_ROUNDS: u8 = 5; // automatic rounds (action -> result -> agent) per question

/// Opens the main window.
pub fn run() -> eframe::Result {
    // When installed, the app starts in any folder: its JSON files go to the user's data folder.
    let dir = storage::data_dir();
    if storage::create_private_dir(&dir).and_then(|_| std::env::set_current_dir(&dir)).is_err() {
        eprintln!("{}", tr!("No se pudo usar {}; los datos quedan en la carpeta actual.", "Could not use {}; data stays in the current folder.", dir.display()));
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("mishapi")
            .with_inner_size([1440.0, 900.0])
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../assets/png/logo-original.png")).unwrap_or_default()),
        ..Default::default()
    };
    eframe::run_native(
        "mishapi",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            let app = App::new();
            apply_theme(&cc.egui_ctx, app.settings.light_theme);
            Ok(Box::new(app))
        }),
    )
}

#[derive(Clone, Copy, PartialEq, Default)]
enum Tab {
    #[default]
    Params,
    Auth,
    Headers,
    Body,
    Pre,
    Tests,
    Contract,
}

#[derive(Clone, Copy, PartialEq, Default)]
enum RespTab {
    #[default]
    Body,
    Headers,
    Cookies,
    Tests,
    Console,
}

#[derive(Clone, Copy, PartialEq, Default)]
enum Side {
    #[default]
    Cols,
    Envs,
    History,
}

#[derive(Clone)]
enum Confirm {
    Env(String),
    Col(usize),
    Req(usize, usize),
    CloseTab(usize),
}

#[derive(Clone, Copy)]
enum AuthTarget {
    /// Request tab (index into `tabs`).
    Tab(usize),
    Collection(usize),
}

/// Inactive request tab (the active one lives in App's fields: ed, cur, resp...).
#[derive(Default)]
struct TabState {
    cur: Option<(usize, usize)>,
    /// To relocate the request if something before it was deleted.
    col_name: String,
    col_sel: usize,
    ed: Request,
    resp: Option<Result<Response, String>>,
    resp_view: String,
    tests: Vec<TestResult>,
    logs: Vec<String>,
    tab: Tab,
}

struct Hist {
    req: Request,
    status: Option<u16>,
    ms: u64,
    at: Instant,
}

struct ChatMsg {
    /// "user" | "assistant" | "tool" (action results; sent to the agent as a user message)
    role: &'static str,
    shown: String,
    sent: String,
    error: bool,
}

/// State before a question, to undo what the agent did.
struct Snapshot {
    cols: Vec<Collection>,
    batteries: Vec<model::Battery>,
    envs: Environments,
    ed: Request,
    cur: Option<(usize, usize)>,
    col_sel: usize,
}

/// Contract diffs and validation warnings for the open request (recomputed only when it changes).
struct ContractView {
    parts: ReqParts,
    key: String,
    changes: Vec<overlay::Change>,
    issues: Vec<String>,
}

/// In-flight send, with the environment and collection that script-changed variables must go back to.
struct Pending {
    rx: Receiver<SendDone>,
    req: Request,
    env: String,
    col: Option<(usize, String)>,
}

enum RunMsg {
    Step(runner::Step),
    Progress(String),
    RunDone(Scopes),
    StressDone(Result<Vec<runner::StageReport>, String>),
}

/// Window for the collection runner and stress batteries.
#[derive(Default)]
struct RunPanel {
    open: bool,
    stress: bool,
    col: usize,
    iterations: u32,
    bail: bool,
    /// Endpoint to stress; None = the whole collection.
    target: Option<usize>,
    /// Index into settings.batteries; if equal to its length, it is the custom one.
    battery: usize,
    vus: u32,
    secs: u64,
    /// Warm-up ramp of the custom battery (0 = no ramp).
    ramp_secs: u64,
    /// Setup request (login) sent once before measuring.
    setup: Option<usize>,
    /// Rows of the runner's data file (one per iteration).
    data: Vec<Vars>,
    data_name: String,
    rx: Option<Receiver<RunMsg>>,
    stop: Arc<AtomicBool>,
    /// Environment and collection that variables changed by runner scripts go back to.
    dest: (String, Option<(usize, String)>),
    steps: Vec<runner::Step>,
    progress: String,
    reports: Vec<runner::StageReport>,
    err: String,
}

struct App {
    client: reqwest::blocking::Client,
    cols: Vec<Collection>,
    envs: Environments,
    agents: Vec<AgentCfg>,
    settings: Settings,
    blocked: Vec<&'static str>, // files that could not be read: they are not overwritten
    error: String,

    cur: Option<(usize, usize)>, // open request: (collection, index)
    ed: Request,
    col_sel: usize,
    tab: Tab,
    side: Side,
    filter: String,
    /// Environment being edited in the Environments section (not always the active one).
    env_edit: String,
    env_rows: Vec<Row>,
    /// Open collection-variables window and its table.
    vars_win: Option<usize>,
    /// A collection's authorization window.
    auth_win: Option<usize>,
    show_cookies: bool,
    /// "Add cookie" form: domain, name, value.
    new_cookie: (String, String, String),
    /// In-flight OAuth 2.0 "Get token" and who the token goes to.
    oauth_pending: Option<(Receiver<Result<auth::Token, String>>, AuthTarget)>,
    col_rows: Vec<Row>,
    /// Headers as text instead of a table.
    bulk: bool,
    tabs: Vec<TabState>,
    active_tab: usize,
    history: Vec<Hist>,
    resp_tab: RespTab,
    raw: bool,
    new_env: String,
    new_col: String,

    sending: Option<Pending>,
    resp: Option<Result<Response, String>>,
    resp_view: String,
    tests: Vec<TestResult>,
    logs: Vec<String>,

    show_import: bool,
    import_text: String,
    import_url: String,
    /// In-flight contract download: (text, source URL) and whether the agent requested it.
    import_pending: Option<(Receiver<Result<(String, Option<String>), String>>, bool)>,
    import_keep_scripts: bool,
    import_err: String,
    confirm: Option<Confirm>,

    agent_sel: usize,
    chat: Vec<ChatMsg>,
    question: String,
    with_context: bool,
    asking: Option<Receiver<Result<String, String>>>,
    show_agents_cfg: bool,
    agents_json: String,
    agents_err: String,
    timeout_edit: u64,

    show_repos: bool,
    run: RunPanel,
    repo_url: String,
    repo_branch: String,
    repo_scan: Option<repo::Scan>,
    /// In-flight connection and, if it came from "Sync" on a collection, which one.
    repo_pending: Option<(Receiver<Result<repo::Scan, String>>, Option<usize>)>,
    repo_msg: String,
    notice: String,

    agent_rounds: u8,
    /// The agent requested a send and waits for its result to continue.
    agent_waits_send: bool,
    /// The agent started a run (runner, stress or import) and waits for its result.
    agent_waits_task: bool,
    /// Agent actions awaiting approval (deletions or sends to a new host).
    pending_actions: Vec<Action>,
    undo: Option<Snapshot>,
    /// Hosts YOU sent to or that come from an imported contract: the agent may send there without asking.
    trusted_hosts: HashSet<String>,
    contract_view: Option<ContractView>,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if cookies::take_dirty() {
            self.persist(cookies::FILE);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Enter)) {
            if ctx.memory(|m| m.has_focus(Id::new("question"))) {
                self.ask(ctx);
            } else {
                self.send_by_user(ctx);
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::S)) {
            self.save_request();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::T)) {
            self.new_tab();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::W)) {
            self.request_close_tab(self.active_tab);
        }

        let bar = |fill: Color32| egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(12, 6));
        egui::TopBottomPanel::top("top").frame(bar(theme::deep())).show(ctx, |ui| self.top_bar(ui));
        if !self.error.is_empty() || !self.notice.is_empty() {
            egui::TopBottomPanel::top("banner").frame(bar(theme::surface())).show(ctx, |ui| {
                for (text, color) in [(&mut self.error, theme::err()), (&mut self.notice, theme::ok())] {
                    if text.is_empty() {
                        continue;
                    }
                    egui::Frame::new().fill(color.gamma_multiply(0.12)).stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.5))).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(if color == theme::err() { "✖" } else { "✔" }).color(color).strong());
                            ui.add(egui::Label::new(RichText::new(text.as_str()).color(color)).wrap());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(Button::new("×").frame(false)).clicked() {
                                    text.clear();
                                }
                            });
                        });
                    });
                }
            });
        }
        egui::TopBottomPanel::bottom("status").frame(bar(theme::deep())).show(ctx, |ui| self.status_bar(ui));
        egui::SidePanel::left("side")
            .default_width(300.0)
            .width_range(220.0..=520.0)
            .frame(egui::Frame::new().fill(theme::panel()).inner_margin(egui::Margin::symmetric(10, 4)))
            .show(ctx, |ui| self.sidebar(ui));
        if self.settings.agent_panel {
            egui::SidePanel::right("agent")
                .default_width(380.0)
                .frame(egui::Frame::new().fill(theme::panel()).inner_margin(10.0))
                .show(ctx, |ui| self.agent_panel(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::surface()).inner_margin(egui::Margin { left: 14, right: 14, top: 8, bottom: 8 })).show(ctx, |ui| self.main_panel(ui));
        self.windows(ctx);
    }

    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        theme::panel().to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_session();
    }
}
