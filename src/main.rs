mod daemon;
mod fb;
mod fb_client;
mod font;
mod hooks;
mod img;
mod state;
mod touch;

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let res = match cmd.as_str() {
        "daemon" => daemon::run(),
        "hook-start" => hooks::send("start", true),
        "hook-question" => hooks::send("question", false),
        "hook-answer" => hooks::send("answer", false),
        "hook-stop" => hooks::send("stop", false),
        "hook-end" => hooks::send("end", false),
        "status" => hooks::status(),
        "quit" => hooks::quit(),
        _ => {
            eprintln!(
                "usage: touch-claude <daemon|hook-start|hook-question|hook-answer|hook-stop|hook-end|status|quit>"
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = res {
        eprintln!("touch-claude: {e:#}");
        std::process::exit(1);
    }
}
