use arboard::Clipboard;
use serde::{Deserialize, Serialize};
use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const SOCKET_PATH: &str = "/tmp/iosync_socket";
const LOG_PATH: &str = "/tmp/ssh-clipboard.log";

//Write a macro to log to a file
macro_rules! log {
    ($($arg:tt)*) => {
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(LOG_PATH)
                .expect("Failed to open log file");
            writeln!(file, $($arg)*).expect("Failed to write to log file");
        }
    };
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct Message {
    content: String,
}

/// Helper: remove old socket if it exists.
fn cleanup_socket() {
    if Path::new(SOCKET_PATH).exists() {
        let _ = std::fs::remove_file(SOCKET_PATH);
    }
}

fn run_iosync_mode_on_linux(last_message: Arc<Mutex<String>>) -> io::Result<()> {
    cleanup_socket();
    let listener = UnixListener::bind(SOCKET_PATH)?;
    log!("Listening on the Unix socket: {}", SOCKET_PATH);

    // Server loop: accept connections on the Unix socket.
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let last_message_conn = Arc::clone(&last_message);
                let unix_socket_reader_thread = thread::spawn(move || {
                    // Read the command from the client.
                    let mut reader = BufReader::new(&mut stream);
                    let mut command = String::new();
                    if let Err(e) = reader.read_line(&mut command) {
                        log!("Failed to read from stream: {}", e);
                        return;
                    }
                    command = command.trim().to_string();
                    log!("Received command: {}", command);

                    // Command protocol:
                    // "GET" returns the current clipboard content.
                    // "SET <text>" updates the clipboard.
                    if command == "GET" {
                        let last = last_message_conn.lock().unwrap();
                        let reply = last.clone();
                        let _ = stream.write_all(reply.as_bytes());
                    } else if command.starts_with("SET ") {
                        let new_text = command["SET ".len()..].to_string();
                        let msg = Message {
                            content: new_text.clone(),
                        };
                        let mut last = last_message_conn.lock().unwrap();
                        if *last != msg.content {
                            if let Ok(msg_str) = serde_json::to_string(&msg) {
                                *last = msg.content.clone();
                                eprintln!("CLIPBOARD-SYNC:{}", msg_str)
                            }
                        }
                        let _ = stream.write_all(b"OK");
                    } else {
                        let _ = stream.write_all(b"Unknown command");
                    }
                    let _ = stream.shutdown(Shutdown::Both);
                });
                unix_socket_reader_thread.join().expect("Unix socket reader thread panicked");
            }
            Err(e) => {
                log!("Socket connection failed: {}", e);
            }
        }
    }
    return Ok(());
}

fn run_iosync_mode_on_mac(last_message: Arc<Mutex<String>>) -> io::Result<()> {
    // Thread that monitors the clipboard changes.
    let last_message_for_clipboard = Arc::clone(&last_message);
    let clipboard_thread = thread::spawn(move || {
        let mut clipboard = Clipboard::new().expect("Failed to open clipboard");
        loop {
            thread::sleep(Duration::from_millis(200));
            if let Ok(text) = clipboard.get_text() {
                let mut last = last_message_for_clipboard.lock().unwrap();
                if *last != text {
                    log!("Clipboard changed: {}", text);
                    let msg = Message {
                        content: text.clone(),
                    };
                    if let Ok(msg_str) = serde_json::to_string(&msg) {
                        *last = text;
                        eprintln!("CLIPBOARD-SYNC:{}", msg_str);
                    }
                }
            }
        }
    });

    let last_message_for_stdin = Arc::clone(&last_message);
    let stdin_thread = thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            if let Ok(line) = line {
                // Check if the line starts with "CLIPBOARD_SYNC:".
                if line.starts_with("CLIPBOARD_SYNC:") {
                    // Extract the message after the command.
                    let msg_str = line["CLIPBOARD_SYNC:".len()..].trim().to_string();
                    if let Ok(msg) = serde_json::from_str::<Message>(&msg_str) {
                        let mut last = last_message_for_stdin.lock().unwrap();
                        if *last != msg.content {
                            log!("Setting clipboard to: {}", msg.content);
                            *last = msg.content.clone();
                            let mut clipboard = Clipboard::new().expect("Failed to open clipboard");
                            let _ = clipboard.set_text(msg.content);
                        }
                    }
                } else {
                    println!("{}", line);
                }
            }
        }
    });

    clipboard_thread.join().expect("Clipboard thread panicked");
    stdin_thread.join().expect("Stdin thread panicked");

    return Ok(());
}

/// The iosync mode: run a server on a Unix domain socket and monitor the clipboard.
fn run_iosync_mode() -> io::Result<()> {
    // Shared state for the most recent clipboard message.
    let last_message = Arc::new(Mutex::new(String::new()));
    if cfg!(target_os = "linux") {
        // Listen on the Unix domain socket if we are running inside
        // a Linux box
        // The assumption is that you are sshing into a Linux box that doesn't have a GUI
        // Thus we are using the xclip mode to notify this server of clipboard changes
        return run_iosync_mode_on_linux(last_message);
    } else {
        // Listen to macOS clipboard changes
        return run_iosync_mode_on_mac(last_message);
    }
}

/// The xclip mode: act as a client that either reads (with "-o") or writes to the socket.
fn run_xclip_mode() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    // Connect to the Unix domain socket.
    match UnixStream::connect(SOCKET_PATH) {
        Ok(mut stream) => {
            if args.len() > 1 && args[1] == "-o" {
                // Read mode: send "GET" and print the reply.
                stream.write_all(b"GET\n")?;
                let mut reply = String::new();
                stream.read_to_string(&mut reply)?;
                println!("{}", reply);
            } else {
                // Write mode: read from stdin, then send "SET <input>".
                let stdin = io::stdin();
                let input: String = stdin
                    .lock()
                    .lines()
                    .filter_map(Result::ok)
                    .collect::<Vec<_>>()
                    .join("\n");
                let cmd = format!("SET {}", input);
                stream.write_all(cmd.as_bytes())?;
                let mut reply = String::new();
                stream.read_to_string(&mut reply)?;
            }
            Ok(())
        }
        Err(e) => {
            log!("Failed to connect to the iosync socket: {}", e);
            return Err(e);
        }
    }
}

fn main() {
    // Decide mode based on the executable name.
    let exe_name = env::args().next().unwrap_or_default();
    if exe_name.ends_with("xclip") {
        log!("Running in xclip mode");
        if let Err(err) = run_xclip_mode() {
            log!("Error in xclip mode: {}", err);
            std::process::exit(1);
        }
    } else {
        log!("Running in iosync mode");
        if let Err(err) = run_iosync_mode() {
            log!("Error in iosync mode: {}", err);
            std::process::exit(1);
        }
    }
}
