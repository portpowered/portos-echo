mod peer;

use echo_agent_api::Task;
use echo_contracts::{ids::TaskId, task::TaskEvent};
use futures_util::FutureExt as _;
use serde_json::{Value, json};

use crate::{MuseLink, MuseService as _, ServiceError};

use self::peer::{Chat, connect, event, expect_reset, invoke, progress, send_ack, send_request, subscribe};
use super::{Fake, ManualClock, Pki, auth, demo_task, fake::Vm};

const NOW: u64 = 1_700_000_000;

async fn fixture() -> (Fake, Pki, ManualClock, MuseLink, Vm, i64) {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    let clock = ManualClock::new(NOW);
    let link = auth::start(&fake, &pki, &clock);
    let (vm, control) = connect(&fake, &link).await;
    (fake, pki, clock, link, vm, control)
}

async fn start_chat(fake: &Fake, link: &MuseLink, vm: &mut Vm, task: Task) -> (TaskId, Chat) {
    let (id, chat) = tokio::join!(link.submit(task), async {
        let (send, request) = send_request(vm).await;
        let session = request["session_id"].as_str().unwrap();
        let snapshot: Value = serde_json::from_slice(&std::fs::read(fake.task_path()).unwrap()).unwrap();
        let record = snapshot["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["session_id"] == session)
            .unwrap();
        let id = record["task_id"].as_str().unwrap();
        assert_eq!(record["state"], "live");
        assert_eq!(record["deadline_unix_seconds"], NOW + 2);
        assert!(
            request["message"]
                .as_str()
                .unwrap()
                .contains(&format!("Task ID: {id}\n"))
        );
        send_ack(vm, send, session).await;
        subscribe(vm, session).await
    });
    (id.unwrap(), chat)
}

#[tokio::test]
async fn t3_matching_report_completes_scoped_chat() {
    let (fake, _, _, link, mut vm, control) = fixture().await;
    let (first, chat) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    let (second, other) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    assert_ne!(chat.session, other.session);
    vm.chunk(
        chat.stream,
        event(
            &other.session,
            10,
            "delta.text_append",
            json!({"message_id":"other","text":"wrong chat"}),
        ),
        false,
    )
    .await;
    chat.emit(
        &mut vm,
        1,
        "delta.message_start",
        json!({"message_id":"reply","reply_to_message_id":"user-1"}),
    )
    .await;
    let text = event(
        &chat.session,
        2,
        "delta.text_append",
        json!({"message_id":"reply","text":"Checking ✓"}),
    );
    let split = text.iter().position(|byte| *byte == 0xe2).unwrap() + 1;
    vm.chunk(chat.stream, text[..split].to_vec(), false).await;
    vm.chunk(chat.stream, text[split..].to_vec(), false).await;
    assert_eq!(link.next_event(&first).await, Ok(Some(progress(&first, "Checking ✓"))));
    assert!(link.next_event(&second).now_or_never().is_none());
    chat.emit(
        &mut vm,
        3,
        "delta.message_done",
        json!({"message_id":"reply","display_text":"Checking ✓"}),
    )
    .await;
    assert_eq!(
        invoke(&mut vm, control, "complete", &first, "completed", "Fixture read").await,
        json!({"method":"link.result","id":"complete","ok":true,"payload":{"accepted":true,"task_id":first}})
    );
    assert_eq!(
        link.next_event(&first).await,
        Ok(Some(TaskEvent::Completed {
            task_id: first.clone(),
            summary: "Fixture read".into()
        }))
    );
    assert_eq!(link.next_event(&first).await, Ok(None));
    assert!(link.next_event(&second).now_or_never().is_none());
    expect_reset(&mut vm, chat.stream).await;
}

#[tokio::test]
async fn t3_unknown_report_leaves_live_task_pending() {
    let (fake, _, _, link, mut vm, control) = fixture().await;
    let (id, chat) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    let unknown = TaskId::new("task-absent").unwrap();
    assert_refused(
        &invoke(&mut vm, control, "unknown", &unknown, "completed", "wrong task").await,
        "unknown",
    );
    assert!(link.next_event(&id).now_or_never().is_none());
    assert_eq!(
        invoke(&mut vm, control, "known", &id, "completed", "Matched").await["ok"],
        true
    );
    assert_eq!(
        link.next_event(&id).await,
        Ok(Some(TaskEvent::Completed {
            task_id: id.clone(),
            summary: "Matched".into()
        }))
    );
    assert_eq!(link.next_event(&id).await, Ok(None));
    expect_reset(&mut vm, chat.stream).await;
}

#[tokio::test]
async fn t3_reported_failure_and_duplicate_are_correlated() {
    let (fake, _, _, link, mut vm, control) = fixture().await;
    let (id, chat) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    let response = invoke(&mut vm, control, "failure", &id, "failed", "Fixture refused").await;
    assert_eq!(response["id"], "failure");
    assert_eq!(response["payload"]["task_id"], json!(id));
    expect_reset(&mut vm, chat.stream).await;
    assert_refused(
        &invoke(&mut vm, control, "duplicate", &id, "completed", "late").await,
        "duplicate",
    );
    assert_eq!(
        link.next_event(&id).await,
        Ok(Some(TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: "Fixture refused".into()
        }))
    );
    assert_eq!(link.next_event(&id).await, Ok(None));
}

#[tokio::test]
async fn t3_deadline_at_two_seconds_is_nonretryable() {
    let (fake, _, clock, link, mut vm, control) = fixture().await;
    let (id, chat) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    vm.chunk(
        chat.stream,
        event(
            &chat.session,
            1,
            "message.assistant",
            json!({"message_id":"reply","display_text":"Done"}),
        ),
        false,
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
    clock.set(NOW + 1);
    assert!(link.next_event(&id).now_or_never().is_none());
    clock.set(NOW + 2);
    assert_eq!(
        link.next_event(&id).await,
        Ok(Some(TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: "Muse did not report a result in time".into()
        }))
    );
    expect_reset(&mut vm, chat.stream).await;
    assert_refused(
        &invoke(&mut vm, control, "late", &id, "completed", "too late").await,
        "late",
    );
    assert_eq!(link.next_event(&id).await, Ok(None));
}

#[tokio::test]
async fn t3_cancel_advises_original_chat_and_ignores_report() {
    let (fake, _, _, link, mut vm, control) = fixture().await;
    let (id, chat) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    let (other, other_chat) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    assert_eq!(link.cancel(&id).await, Ok(()));
    expect_reset(&mut vm, chat.stream).await;
    let (advisory, body) = send_request(&mut vm).await;
    assert_eq!(body["session_id"], chat.session);
    assert_eq!(
        body["message"],
        format!("Cancel task {id}. Do not call echo.report.result for it.")
    );
    send_ack(&mut vm, advisory, &chat.session).await;
    vm.chunk(
        chat.stream,
        event(
            &chat.session,
            1,
            "message.assistant",
            json!({"message_id":"late","content":"late"}),
        ),
        false,
    )
    .await;
    assert_refused(
        &invoke(&mut vm, control, "cancelled", &id, "completed", "ignored").await,
        "cancelled",
    );
    assert_eq!(link.next_event(&id).await, Ok(None));
    vm.chunk(
        other_chat.stream,
        event(
            &other_chat.session,
            1,
            "message.assistant",
            json!({"message_id":"other","content":"Still working"}),
        ),
        false,
    )
    .await;
    assert_eq!(
        link.next_event(&other).await,
        Ok(Some(progress(&other, "Still working")))
    );
}

#[tokio::test]
async fn t3_chat_refusal_keeps_control_live() {
    let (fake, _, _, link, mut vm, control) = fixture().await;
    let (live, chat) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    for status in [403, 200] {
        let (result, refused) = tokio::join!(link.submit(demo_task()), async {
            let (stream, body) = send_request(&mut vm).await;
            let id = body["message"]
                .as_str()
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("Task ID: "))
                .unwrap();
            vm.response(stream, status, json!({"session_id":"wrong","is_thread":true}), true)
                .await;
            TaskId::new(id).unwrap()
        });
        assert_eq!(result, Err(ServiceError::PeerUnavailable));
        assert_eq!(link.next_event(&refused).await, Ok(None));
        assert_refused(
            &invoke(&mut vm, control, "refused", &refused, "completed", "ignored").await,
            "refused",
        );
    }
    assert_eq!(
        invoke(&mut vm, control, "live", &live, "completed", "Done").await["ok"],
        true
    );
    assert_eq!(
        link.next_event(&live).await,
        Ok(Some(TaskEvent::Completed {
            task_id: live,
            summary: "Done".into()
        }))
    );
    expect_reset(&mut vm, chat.stream).await;
}

#[tokio::test]
async fn t3_restart_keeps_task_without_chat_replay() {
    let (fake, pki, clock, link, mut vm, control) = fixture().await;
    let (live, _) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    let (old, chat) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    assert_eq!(
        invoke(&mut vm, control, "old", &old, "completed", "Old result").await["ok"],
        true
    );
    assert_eq!(
        link.next_event(&old).await,
        Ok(Some(TaskEvent::Completed {
            task_id: old.clone(),
            summary: "Old result".into()
        }))
    );
    expect_reset(&mut vm, chat.stream).await;
    link.stop().await.unwrap();
    vm.expect_closed().await;
    drop(link);

    let link = auth::start(&fake, &pki, &clock);
    let (mut vm, control) = connect(&fake, &link).await;
    vm.barrier().await;
    assert_eq!(link.next_event(&old).await, Ok(None));
    assert!(link.next_event(&live).now_or_never().is_none());
    assert_refused(
        &invoke(&mut vm, control, "old-again", &old, "completed", "replay").await,
        "old-again",
    );
    assert_eq!(
        invoke(&mut vm, control, "recovered", &live, "completed", "Recovered").await["ok"],
        true
    );
    assert_eq!(
        link.next_event(&live).await,
        Ok(Some(TaskEvent::Completed {
            task_id: live,
            summary: "Recovered".into()
        }))
    );
}

#[tokio::test]
async fn t3_chat_reset_and_replay_are_isolated() {
    let (fake, _, _, link, mut vm, control) = fixture().await;
    let (live, chat) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    let (other, adjacent) = start_chat(&fake, &link, &mut vm, demo_task()).await;
    vm.reset(chat.stream).await;
    vm.chunk(
        adjacent.stream,
        event(
            &chat.session,
            1,
            "message.assistant",
            json!({"message_id":"old","content":"wrong"}),
        ),
        false,
    )
    .await;
    adjacent
        .emit(&mut vm, 2, "message.user", json!({"message_id":"another-user"}))
        .await;
    adjacent
        .emit(
            &mut vm,
            3,
            "message.assistant",
            json!({"message_id":"other-reply","content":"unrelated user"}),
        )
        .await;
    adjacent
        .emit(
            &mut vm,
            1,
            "delta.message_start",
            json!({"message_id":"foreign","reply_to_message_id":"unrelated"}),
        )
        .await;
    adjacent
        .emit(
            &mut vm,
            2,
            "delta.text_append",
            json!({"message_id":"foreign","text":"wrong parent"}),
        )
        .await;
    let text = event(
        &adjacent.session,
        3,
        "delta.text_append",
        json!({"message_id":"reply","reply_to_message_id":"user-1","text":"Adjacent"}),
    );
    vm.chunk(adjacent.stream, text.clone(), false).await;
    vm.chunk(adjacent.stream, text, false).await;
    assert_eq!(link.next_event(&other).await, Ok(Some(progress(&other, "Adjacent"))));
    vm.barrier().await;
    assert!(link.next_event(&other).now_or_never().is_none());
    assert!(link.next_event(&live).now_or_never().is_none());
    assert_eq!(
        invoke(&mut vm, control, "reset", &live, "completed", "After reset").await["ok"],
        true
    );
    assert_eq!(
        link.next_event(&live).await,
        Ok(Some(TaskEvent::Completed {
            task_id: live,
            summary: "After reset".into()
        }))
    );
}

#[tokio::test]
async fn t3_chat_reset_before_ack_keeps_task_eligible() {
    let (_fake, _, _, link, mut vm, control) = fixture().await;
    let (id, ()) = tokio::join!(link.submit(demo_task()), async {
        let (stream, body) = send_request(&mut vm).await;
        let session = body["session_id"].as_str().unwrap();
        vm.chunk(
            stream + 1,
            event(
                session,
                1,
                "message.assistant",
                json!({"message_id":"premature","content":"before ack"}),
            ),
            false,
        )
        .await;
        vm.reset(stream).await;
    });
    let id = id.unwrap();
    assert!(link.next_event(&id).now_or_never().is_none());
    vm.barrier().await;
    assert_eq!(
        invoke(&mut vm, control, "ambiguous", &id, "completed", "Reported").await["ok"],
        true
    );
    assert_eq!(
        link.next_event(&id).await,
        Ok(Some(TaskEvent::Completed {
            task_id: id.clone(),
            summary: "Reported".into()
        }))
    );
    assert_eq!(link.next_event(&id).await, Ok(None));
}

fn assert_refused(value: &Value, id: &str) {
    assert_eq!(
        *value,
        json!({"method":"link.result","id":id,"ok":false,"error":"unknown task_id"})
    );
}
