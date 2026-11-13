mod app;
mod apple;
mod compose;
mod docker;
mod files;
mod http;
mod logs;
mod runtime;
mod settings;
mod spinner;
mod stats;
mod ui;
mod update;

use app::App;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use std::io::IsTerminal;
use std::sync::mpsc;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--version") => {
            println!("dockwatch {}", update::CURRENT);
            return Ok(());
        }
        Some("update") => {
            if let Err(message) = update_command() {
                eprintln!("dockwatch: {message}");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(_) => {
            eprintln!("Usage: dockwatch [update | --version]");
            std::process::exit(2);
        }
    }
    let local = std::env::current_dir().ok().and_then(|folder| compose::local_project(&folder));
    let (tx, rx) = mpsc::channel();
    if update::enabled() {
        update_in_background(tx.clone());
    }
    let mut app = App::new(tx, local);
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &rx);
    ratatui::restore();
    result
}

fn update_command() -> Result<(), String> {
    if !update::enabled() {
        return Err("Only release builds update themselves. Rebuild from source instead.".into());
    }
    // `off` only stops the automatic check, so an explicit update with it follows stable releases.
    let channel = settings::load().update_channel;
    let progress = spinner::Spinner::start();
    let download_started = std::cell::OnceCell::new();
    let installed = update::install_now(&channel, &|step| match step {
        update::Progress::Checking => progress.set("Check for updates", String::new(), String::new()),
        update::Progress::Downloading { version, received, total } => {
            let elapsed = download_started.get_or_init(std::time::Instant::now).elapsed();
            progress.set(&format!("Download v{version}"), spinner::transfer(received, total, elapsed), spinner::mib(received));
        }
        update::Progress::Verifying => progress.set("Verify the download", String::new(), String::new()),
        update::Progress::Installing => progress.set("Replace the binary", String::new(), String::new()),
    });
    progress.finish(installed.is_ok());
    let styled = std::io::stdout().is_terminal();
    let current = update::CURRENT;
    match installed? {
        Some(version) => println!(
            "\n{} {} → {}  {}",
            paint(styled, spinner::BOLD, "dockwatch"),
            paint(styled, spinner::DIM, &format!("v{current}")),
            paint(styled, &format!("{}{}", spinner::BOLD, spinner::GREEN), &format!("v{version}")),
            paint(styled, spinner::DIM, &format!("{}/tag/v{version}", update::RELEASES)),
        ),
        None => println!("\n{} {} is up to date.", paint(styled, spinner::BOLD, "dockwatch"), paint(styled, spinner::GREEN, &format!("v{current}"))),
    }
    Ok(())
}

fn paint(styled: bool, style: &str, text: &str) -> String {
    if styled { format!("{style}{text}{}", spinner::RESET) } else { text.to_string() }
}

fn update_in_background(tx: mpsc::Sender<app::Msg>) {
    let channel = settings::load().update_channel;
    std::thread::spawn(move || {
        let (text, error) = match update::install_latest(&settings::update_stamp(), &channel) {
            Ok(Some(version)) => (format!("Updated to v{version}. Restart dockwatch to use it."), false),
            Ok(None) => return,
            Err(error) => (format!("Update failed: {error}"), true),
        };
        let _ = tx.send(app::Msg::Notice(text, error));
    });
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App, rx: &mpsc::Receiver<app::Msg>) -> std::io::Result<()> {
    while !app.quit {
        app.tick();
        terminal.draw(|frame| ui::draw(frame, app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => app.key(key),
                Event::Resize(..) => {}
                _ => continue,
            }
        }
        while let Ok(msg) = rx.try_recv() {
            app.handle(msg);
        }
        if let Some(mut command) = app.shell.take() {
            ratatui::restore();
            let status = command.status();
            // Apple's `container exec -it` exits with the terminal left non-blocking, and the next read or
            // write fails with EAGAIN. Its own open of /dev/tty instead breaks crossterm's kqueue.
            for fd in 0..=2 {
                // SAFETY: fcntl on the process's standard descriptors only reads and sets file status flags.
                unsafe { libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) & !libc::O_NONBLOCK) };
            }
            // Exit codes 125 to 127 are docker's own: no such container, no shell in the image.
            if status.as_ref().is_ok_and(|status| matches!(status.code(), Some(125..=127))) {
                eprintln!("\nPress Enter to return to dockwatch.");
                let _ = std::io::stdin().read_line(&mut String::new());
            }
            // ratatui::init would install one more panic hook per shell.
            enable_raw_mode()?;
            execute!(std::io::stdout(), EnterAlternateScreen)?;
            terminal.clear()?;
            if let Err(error) = status {
                app.shell_failed(format!("Cannot run docker exec: {error}"));
            }
        }
    }
    Ok(())
}
