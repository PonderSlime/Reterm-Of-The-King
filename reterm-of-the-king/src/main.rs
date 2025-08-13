use std::{ffi::CString, io::Write, os::unix::io::RawFd, thread, time::Duration};

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::{egui, egui::TextStyle};
use nix::{
    pty::{forkpty, Winsize},
    unistd::{execvp, read, ForkResult, write as nix_write},
};

fn spawn_shell_pty() -> RawFd {
    // (Optional) set initial window size for the PTY so apps see a sane rows/cols.
    let ws = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    match nix::pty::forkpty(&ws, None) {
        Ok(fork_pty_res) => {
            let master_fd = fork_pty_res.master;
            if let ForkResult::Child = fork_pty_res.fork_result {
                // In child: replace with the shell, inheriting the slave as stdio.
                let shell = std::env::var("SHELL").unwrap_or("/bin/bash".to_owned());
                let c = CString::new(shell.clone()).unwrap();
                execvp(&c, &[c.as_c_str()]).expect("execvp(shell) failed");

            }
            master_fd
        }
        Err(e) => panic!("forkpty failed: {e:?}"),
    }
}

fn start_reader_thread(master_fd: RawFd, tx: Sender<Vec<u8>>) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match read(master_fd, &mut buf) {
                Ok(0) => {
                    let _ = tx.send(b"\n[child closed]\n".to_vec());
                    break;
                }
                Ok(n) => {
                    let _ = tx.send(buf[..n].to_vec());
                }
                Err(_e) => {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
    });
}

struct RetermApp {
    master_fd: RawFd,
    rx: Receiver<Vec<u8>>,
    buffer: String,
    // crude scrollback cap to keep memory sane
    max_len: usize,
}

impl RetermApp {
    fn new(master_fd: RawFd, rx: Receiver<Vec<u8>>) -> Self {
        Self {
            master_fd,
            rx,
            buffer: String::new(),
            max_len: 512_000, // ~0.5MB
        }
    }

    fn pump_rx(&mut self) {
        // Drain available chunks each frame
        while let Ok(chunk) = self.rx.try_recv() {
            self.buffer.push_str(&String::from_utf8_lossy(&chunk));
            if self.buffer.len() > self.max_len {
                let cut = self.buffer.len() - self.max_len;
                self.buffer.drain(..cut);
            }
        }
    }

    fn send_to_pty(&self, bytes: &[u8]) {
        let _ = nix_write(self.master_fd, bytes);
    }
}

impl eframe::App for RetermApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_rx();

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.label("reterm-of-the-king — MVP");
            ui.label("Type here; Ctrl-C, etc. ANSI not parsed yet (raw text).");
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let style = ui.style_mut();
            style.text_styles.insert(TextStyle::Body, egui::FontId::monospace(14.0));

            // Capture text input this frame
            let input = ui.input(|i| i.clone());
            for ev in &input.events {
                match ev {
                    egui::Event::Text(t) => {
                        // Unicode text input
                        self.send_to_pty(t.as_bytes());
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        // Basic keys → bytes
                        use egui::Key::*;
                        match key {
                            Enter => self.send_to_pty(b"\n"),
                            Tab => self.send_to_pty(b"\t"),
                            Backspace => self.send_to_pty(&[0x7f]), // DEL
                            ArrowLeft => self.send_to_pty(&[0x1b, b'[', b'D']),
                            ArrowRight => self.send_to_pty(&[0x1b, b'[', b'C']),
                            ArrowUp => self.send_to_pty(&[0x1b, b'[', b'A']),
                            ArrowDown => self.send_to_pty(&[0x1b, b'[', b'B']),
                            C if modifiers.command || modifiers.ctrl => self.send_to_pty(&[0x03]), // Ctrl-C
                            D if modifiers.command || modifiers.ctrl => self.send_to_pty(&[0x04]), // Ctrl-D
                            L if modifiers.command || modifiers.ctrl => self.send_to_pty(&[0x0c]), // Ctrl-L
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            // Text viewport with scroll
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.buffer)
                        .font(TextStyle::Body) // use monospace set above
                        .desired_width(f32::INFINITY)
                        .desired_rows(40)
                        .interactive(false),
                );
            });
        });

        ctx.request_repaint(); // simple: repaint every frame
    }
}

fn main() -> eframe::Result<()> {
    // 1) PTY + shell
    let master_fd = spawn_shell_pty();

    // 2) Start background reader from PTY
    let (tx, rx) = unbounded::<Vec<u8>>();
    start_reader_thread(master_fd, tx);

    // 3) Launch our own window
    let native_opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0]),
        ..Default::default()
    };


   eframe::run_native(
        "reterm-of-the-king",
        native_opts,
        Box::new(move |_cc| Box::new(RetermApp::new(master_fd, rx))),
    )
}
