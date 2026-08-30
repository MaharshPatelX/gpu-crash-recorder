use std::{path::PathBuf, thread};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::{
    config::AppConfig,
    session::{RecordingSession, recover_incomplete_sessions},
};

#[derive(Debug)]
pub enum RecorderCommand {
    Toggle,
    Start,
    Stop,
    Mark(String),
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum RecorderEvent {
    Idle,
    Starting,
    Recording { directory: PathBuf },
    Finalizing,
    Stopped { directory: PathBuf },
    Recovered { directories: Vec<PathBuf> },
    Error { operation: String, message: String },
    ShutdownComplete,
}

pub struct RecorderWorker {
    pub commands: Sender<RecorderCommand>,
    pub events: Receiver<RecorderEvent>,
}

impl RecorderWorker {
    pub fn spawn(config: AppConfig) -> Result<Self> {
        let (command_sender, command_receiver) = unbounded();
        let (event_sender, event_receiver) = unbounded();
        thread::Builder::new()
            .name("recorder-controller".into())
            .spawn(move || worker_loop(config, command_receiver, event_sender))?;
        Ok(Self {
            commands: command_sender,
            events: event_receiver,
        })
    }
}

fn worker_loop(
    config: AppConfig,
    commands: Receiver<RecorderCommand>,
    events: Sender<RecorderEvent>,
) {
    match recover_incomplete_sessions(&config.sessions_root) {
        Ok(directories) if !directories.is_empty() => {
            let _ = events.send(RecorderEvent::Recovered { directories });
        }
        Err(error) => {
            let _ = events.send(RecorderEvent::Error {
                operation: "recovery".into(),
                message: format!("{error:#}"),
            });
        }
        _ => {}
    }
    let _ = events.send(RecorderEvent::Idle);

    let mut session: Option<RecordingSession> = None;
    while let Ok(command) = commands.recv() {
        match command {
            RecorderCommand::Toggle => {
                if session.is_some() {
                    stop_session(&mut session, &events);
                } else {
                    start_session(&config, &mut session, &events);
                }
            }
            RecorderCommand::Start => {
                if session.is_none() {
                    start_session(&config, &mut session, &events);
                }
            }
            RecorderCommand::Stop => {
                if session.is_some() {
                    stop_session(&mut session, &events);
                }
            }
            RecorderCommand::Mark(detail) => {
                if let Some(active) = session.as_ref() {
                    if let Err(error) = active.add_marker(detail) {
                        let _ = events.send(RecorderEvent::Error {
                            operation: "marker".into(),
                            message: format!("{error:#}"),
                        });
                    }
                }
            }
            RecorderCommand::Shutdown => {
                if session.is_some() {
                    stop_session(&mut session, &events);
                }
                let _ = events.send(RecorderEvent::ShutdownComplete);
                break;
            }
        }
    }
}

fn start_session(
    config: &AppConfig,
    session: &mut Option<RecordingSession>,
    events: &Sender<RecorderEvent>,
) {
    let _ = events.send(RecorderEvent::Starting);
    match RecordingSession::start(config.clone()) {
        Ok(recording) => {
            let directory = recording.directory().to_path_buf();
            *session = Some(recording);
            let _ = events.send(RecorderEvent::Recording { directory });
        }
        Err(error) => {
            let _ = events.send(RecorderEvent::Error {
                operation: "start recording".into(),
                message: format!("{error:#}"),
            });
            let _ = events.send(RecorderEvent::Idle);
        }
    }
}

fn stop_session(session: &mut Option<RecordingSession>, events: &Sender<RecorderEvent>) {
    let Some(recording) = session.take() else {
        return;
    };
    let _ = events.send(RecorderEvent::Finalizing);
    match recording.stop() {
        Ok(directory) => {
            let _ = events.send(RecorderEvent::Stopped { directory });
        }
        Err(error) => {
            let _ = events.send(RecorderEvent::Error {
                operation: "stop recording".into(),
                message: format!("{error:#}"),
            });
        }
    }
    let _ = events.send(RecorderEvent::Idle);
}
