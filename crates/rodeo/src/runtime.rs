use std::io::{self, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::app::{App, Msg, Options};
use crate::model::Snapshot;

const FRAME: Duration = Duration::from_millis(8);
const TICK: Duration = Duration::from_millis(250);
const IDLE: Duration = Duration::from_secs(3600);
const BATCH: usize = 512;

pub struct Runner<B: Backend> {
    pub app: App,
    pub terminal: Terminal<B>,
    out: Box<dyn Write>,
    rx: UnboundedReceiver<Msg>,
    tx: UnboundedSender<Msg>,
    last_draw: Option<Instant>,
    snapshot: Option<Rc<Snapshot>>,
    snapshot_valid: bool,
}

impl<B: Backend> Runner<B>
where
    B::Error: Send + Sync + 'static,
{
    pub fn new(options: Options, backend: B, out: Box<dyn Write>) -> io::Result<Self> {
        let (tx, rx) = unbounded_channel();
        let terminal = Terminal::new(backend).map_err(io::Error::other)?;
        let size = terminal.size().map_err(io::Error::other)?;
        let mut app = App::new(options, tx.clone());
        app.start(Rect::new(0, 0, size.width, size.height));
        let ticker = tx.clone();
        tokio::task::spawn_local(async move {
            let mut interval = tokio::time::interval(TICK);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if ticker.send(Msg::Tick).is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            app,
            terminal,
            out,
            rx,
            tx,
            last_draw: None,
            snapshot: None,
            snapshot_valid: false,
        })
    }

    pub fn sender(&self) -> UnboundedSender<Msg> {
        self.tx.clone()
    }

    pub async fn step(&mut self) -> bool {
        let now = Instant::now();
        let held = self.app.hold(now);
        let wait = match (self.app.dirty, held) {
            (false, _) => IDLE,
            (true, Some(until)) => until.saturating_duration_since(now),
            (true, None) => self
                .last_draw
                .map_or(Duration::ZERO, |at| FRAME.saturating_sub(at.elapsed())),
        };
        if self.app.dirty && held.is_none() && wait.is_zero() {
            self.draw_step();
            return !self.app.quit;
        }
        tokio::select! {
            msg = self.rx.recv() => match msg {
                Some(msg) => {
                    self.handle_msg(msg);
                    for _ in 0..BATCH {
                        match self.rx.try_recv() {
                            Ok(msg) => self.handle_msg(msg),
                            Err(_) => break,
                        }
                    }
                }
                None => self.app.quit = true,
            },
            () = tokio::time::sleep(wait) => {}
        }
        if self.app.dirty
            && self.last_draw.is_none_or(|at| at.elapsed() >= FRAME)
            && self.app.hold(Instant::now()).is_none()
        {
            self.draw_step();
        }
        !self.app.quit
    }

    fn handle_msg(&mut self, msg: Msg) {
        let pure_output = matches!(
            &msg,
            Msg::Output {
                output: Some(hc::Output::Bytes(_)),
                ..
            }
        );
        self.app.output_touched_chrome = false;
        self.app.handle(msg);
        if (!pure_output && self.app.dirty) || self.app.output_touched_chrome || self.app.layout_dirty {
            self.snapshot_valid = false;
            self.snapshot = None;
        }
    }

    pub async fn run(&mut self) {
        while self.step().await {}
        self.app.shutdown();
        self.flush();
        self.app.end_popup().await;
    }

    pub fn draw(&mut self) {
        self.snapshot_valid = false;
        self.snapshot = None;
        self.app.layout_dirty = true;
        self.draw_step();
    }

    fn draw_step(&mut self) {
        if self.app.layout_dirty {
            self.app.relayout();
            self.snapshot_valid = false;
            self.snapshot = None;
        }
        self.app.update_title();
        if self.app.full_redraw {
            self.app.full_redraw = false;
            let _ = self.terminal.clear();
        }
        let snapshot = if self.snapshot_valid
            && let Some(cached) = &self.snapshot
        {
            Rc::clone(cached)
        } else {
            let fresh = Rc::new(self.app.snapshot());
            self.snapshot = Some(Rc::clone(&fresh));
            self.snapshot_valid = true;
            fresh
        };
        let app = &mut self.app;
        if let Err(error) = self
            .terminal
            .draw(|frame| crate::render::draw_with_snapshot(app, frame, &snapshot))
        {
            tracing::error!(%error, "draw failed");
        }
        self.app.dirty = false;
        self.last_draw = Some(Instant::now());
        self.flush();
    }

    fn flush(&mut self) {
        let bytes = self.app.take_output();
        if !bytes.is_empty() {
            let _ = self.out.write_all(&bytes);
        }
        let _ = self.out.flush();
    }
}
