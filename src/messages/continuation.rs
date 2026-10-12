//! The prompt that wakes an agent to read its inbox, however it reaches the
//! agent: a Stop hook's reason, or input pm queues. It is recognised again
//! where pm meets it as input or in a transcript.

const START: &str = "You have new messages";
const END: &str = ". Run `pm msg read` to read them.";

/// The wake-up naming `senders`, oldest first.
pub fn wake_text(senders: &[String]) -> String {
    if senders.is_empty() {
        return format!("{START}{END}");
    }
    format!("{START} from {}{END}", senders.join(", "))
}

/// Whether `prompt` is one [`wake_text`] wrote.
pub fn is_continuation(prompt: &str) -> bool {
    let prompt = prompt.trim();
    prompt == wake_text(&[])
        || prompt
            .strip_prefix(START)
            .and_then(|rest| rest.strip_suffix(END))
            .and_then(|rest| rest.strip_prefix(" from "))
            .is_some_and(|senders| !senders.is_empty() && !senders.contains('\n'))
}
