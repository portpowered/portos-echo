use super::*;
use crate::bridge_mcp::bridge_presentation::Presentation;
use tokio::sync::mpsc;

fn ready() -> UiState {
    UiState::Ready {
        message: "Echo Live ready".into(),
    }
}

#[tokio::test(start_paused = true)]
async fn dispatch_in_flight_shows_thinking_in_silence_and_speaking_wins_while_audio_plays() {
    let directory = tempfile::tempdir().unwrap();
    let mut trace = Trace::open(&directory.path().join("trace"), "chatgpt").unwrap();
    let health = Health::offline();
    let (send, mut receive) = mpsc::channel(16);
    let mut screen = Screen::bind(None).await.unwrap();
    screen.presentation = Presentation::new(Some(send));
    let mut view = View::new(&screen, &health, &mut trace);
    let run = |view: &mut View<'_>, notice| view.observe(ServeNotice::Session(SessionNotice::Run(notice)));
    view.observe(ServeNotice::Listening).unwrap();
    // The dispatch starts in silence, its acknowledgement speaks, then silence
    // again with the turn still running: thinking, speaking, thinking.
    for notice in [Notice::Thinking(true), Notice::ReplyStarted, Notice::ReplyPaused] {
        run(&mut view, notice).unwrap();
    }
    // Completion keeps Thinking until the answer speaks; no idle face between.
    run(&mut view, Notice::Thinking(false)).unwrap();
    for notice in [Notice::ReplyStarted, Notice::ReplyPaused] {
        run(&mut view, notice).unwrap();
    }
    // A dispatch that starts under speech shows only once the mouth closes,
    // and a run that ends while thinking returns to ready.
    for notice in [Notice::ReplyStarted, Notice::Thinking(true), Notice::ReplyPaused] {
        run(&mut view, notice).unwrap();
    }
    view.observe(ServeNotice::Session(SessionNotice::Completed(Stats::default())))
        .unwrap();
    let mut states = vec![];
    while let Ok(state) = receive.try_recv() {
        states.push(state);
    }
    assert_eq!(
        states,
        [
            UiState::Listening,
            UiState::Thinking,
            UiState::Speaking,
            UiState::Thinking,
            UiState::Speaking,
            ready(),
            UiState::Speaking,
            UiState::Thinking,
            ready(),
        ]
    );
}
