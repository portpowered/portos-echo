use echo_contracts::task::TaskEvent;
use futures_util::FutureExt as _;
use serde_json::json;

use crate::{
    MuseService as _,
    support::{Fake, Pki, SAVED_AT, connect, demo_task, event, expect_reset, invoke, progress, start_chat},
};

use super::{ManualClock, start};

#[tokio::test]
async fn t3_deadline_at_two_seconds_is_nonretryable() {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    let clock = ManualClock::new(SAVED_AT);
    let link = start(&fake, &pki, &clock);
    let (mut vm, control) = connect(&fake, &link).await;
    let (id, chat) = start_chat(&fake, &link, &mut vm, demo_task(), || SAVED_AT).await;
    chat.emit(
        &mut vm,
        1,
        "message.assistant",
        json!({"message_id":"reply","display_text":"Done"}),
    )
    .await;
    vm.chunk(
        chat.stream,
        event(
            &chat.session,
            1,
            "task.status",
            json!({"task_id":"external","status":"completed"}),
        ),
        false,
    )
    .await;
    assert_eq!(link.next_event(&id).await, Ok(Some(progress(&id, "Done"))));
    clock.set(SAVED_AT + 1);
    assert!(link.next_event(&id).now_or_never().is_none());
    clock.set(SAVED_AT + 2);
    assert_eq!(
        link.next_event(&id).await,
        Ok(Some(TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: "Muse did not report a result in time".into()
        }))
    );
    expect_reset(&mut vm, chat.stream).await;
    assert_eq!(
        invoke(&mut vm, control, "late", &id, "completed", "too late").await,
        json!({"method":"link.result","id":"late","ok":false,"error":"unknown task_id"})
    );
    assert_eq!(link.next_event(&id).await, Ok(None));
}
