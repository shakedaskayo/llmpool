//! Diagnostic: try to renew one stored login against the real provider and
//! print only the outcome. The login is read from standard input as llmpool
//! stores it (`{"oauth": ..., "account": ...}`); nothing is written anywhere,
//! so a login that does renew is spent. Use it on a login that is already dead
//! to check that the request itself is still what the provider expects.

use llmpool_adapter_claude::oauth;
use llmpool_adapter_claude::profile::Secret;
use std::io::Read;

fn main() {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .expect("a stored login on standard input");
    let mut secret: Secret = serde_json::from_str(&text).expect("a stored login");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    println!("due for renewal: {}", oauth::due(&secret, now_ms));
    match oauth::renew(&mut secret, oauth::TOKEN_URL, now_ms) {
        oauth::Outcome::Renewed => {
            println!("outcome: renewed (this copy is now spent; sign in again)")
        }
        oauth::Outcome::Dead => println!("outcome: the provider says this login has ended"),
        oauth::Outcome::Later(why) => {
            println!("outcome: unclear, would be tried again later ({why})")
        }
    }
}
