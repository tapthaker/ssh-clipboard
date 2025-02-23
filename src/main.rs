use arboard::Clipboard;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct Message {
    content: String,
}

fn main() {
    // Shared state for the most recent clipboard message to avoid ping-pong.
    let last_message = Arc::new(Mutex::new(Message {
        content: String::new(),
    }));

    // Thread that reads from stdin.
    let last_message_clone = Arc::clone(&last_message);
    let stdin_thread = thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            if let Ok(line) = line {
                // Check if the line starts with "CLIPBOARD_SYNC:".
                if line.starts_with("CLIPBOARD_SYNC:") {
                    // Extract the message after the command.
                    let msg_content = line["CLIPBOARD_SYNC:".len()..].trim().to_string();
                    let msg = Message {
                        content: msg_content,
                    };
                    // Serialize the message to a JSON string.
                    let json_msg = serde_json::to_string(&msg)
                        .expect("Failed to serialize message to JSON");
                    let mut last = last_message_clone.lock().unwrap();
                    if *last != msg {
                        *last = msg;
                        let mut clipboard =
                            Clipboard::new().expect("Failed to open clipboard");
                        clipboard
                            .set_text(json_msg)
                            .expect("Failed to set clipboard text");
                    }
                } else {
                    // If the line doesn't match, print it to standard output.
                    println!("{}", line);
                }
            }
        }
    });

    // Thread to monitor the clipboard and send new messages to stderr.
    let last_message_clone2 = Arc::clone(&last_message);
    let clipboard_thread = thread::spawn(move || {
        let mut clipboard = Clipboard::new().expect("Failed to open clipboard");
        loop {
            thread::sleep(Duration::from_millis(200)); // avoid busy looping
            if let Ok(text) = clipboard.get_text() {
                // Attempt to deserialize the JSON string into a Message.
                if let Ok(parsed_msg) = serde_json::from_str::<Message>(&text) {
                    let mut last = last_message_clone2.lock().unwrap();
                    if *last != parsed_msg {
                        *last = parsed_msg.clone();
                        // Print only the message content to stderr.
                        eprintln!("{}", parsed_msg.content);
                    }
                }
            }
        }
    });

    stdin_thread.join().unwrap();
    clipboard_thread.join().unwrap();
}
