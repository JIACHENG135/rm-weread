//! Phase 1 test binary: QR login + shelf fetch, run on a normal machine
//! (not cross-compiled, not on-device) against a real WeRead account —
//! see docs/design.md's phased plan, step 1. No rendering, no QML, no
//! systemd — just proving the login + skill-gateway wire protocol works.
//!
//! Usage: cargo run --bin weread_login [-- --session-path <path>]
//! Defaults to ./session.json in the current directory. If a valid
//! session already exists there, skips straight to the shelf fetch.

use rm_weread::{login, session, shelf};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SESSION_TIMEOUT: Duration = Duration::from_secs(300);

fn session_path() -> PathBuf {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--session-path"
            && let Some(path) = args.next()
        {
            return PathBuf::from(path);
        }
    }
    PathBuf::from("session.json")
}

fn prompt_otp(message: &str) -> String {
    print!("{message}: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).expect("failed to read stdin");
    line.trim().to_string()
}

fn qr_login(agent: &ureq::Agent) -> Result<login::Session, Box<dyn std::error::Error>> {
    let (uid, mut cookies) = login::begin(agent)?;
    println!("打开这个链接（手机上要已登录微信读书）完成扫码登录：");
    println!("  {}", login::confirm_url(&uid));
    println!("等待确认...");

    let started = Instant::now();
    let mut otp: Option<String> = None;
    loop {
        if started.elapsed() > SESSION_TIMEOUT {
            return Err("登录超时（5 分钟），重新运行再试一次".into());
        }
        match login::poll(agent, &uid, &mut cookies, otp.as_deref())? {
            login::PollOutcome::Pending => {
                otp = None;
                continue;
            }
            login::PollOutcome::NeedOtp { retry_message } => {
                let prompt = retry_message.as_deref().unwrap_or("手机上显示了一个 4 位验证码，输入它");
                otp = Some(prompt_otp(prompt));
            }
            login::PollOutcome::Expired => {
                return Err("二维码/验证码已过期，重新运行再试一次".into());
            }
            login::PollOutcome::Done(result) => {
                println!("登录确认成功，正在获取账号信息和 API key...");
                return login::complete(agent, result);
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = session_path();
    let agent = login::agent();

    let session = match session::load(&path) {
        Some(session) => {
            println!("复用已有登录态：{}（{}）", session.account.name, path.display());
            session
        }
        None => {
            let session = qr_login(&agent)?;
            session::save(&path, &session)?;
            println!("登录成功：{}，已保存到 {}", session.account.name, path.display());
            session
        }
    };

    println!("正在拉取书架...");
    let shelf = shelf::sync(&agent, &session.api_key)?;
    println!("书架共 {} 本：", shelf.books.len());
    for book in &shelf.books {
        println!("  - {} / {} ({})", book.title, book.author, book.book_id);
    }

    Ok(())
}
