mod host {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/sim_support/host_owner.rs"));
}
pub use host::Host;

pub struct Completion<T> {
    name: &'static str,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<T>,
}

impl<T: Send + 'static> Completion<T> {
    pub fn new(name: &'static str, shutdown: impl Future<Output = T> + Send + 'static) -> Self {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = stopped.await;
            shutdown.await
        });
        Self {
            name,
            stop: Some(stop),
            task,
        }
    }

    pub fn stop(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
    }

    pub async fn assert_closed(mut self) -> T {
        // Observe under the existing nextest test ceiling; the joined API
        // retains its original per-owner cleanup deadlines.
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), &mut self.task).await;
        if result.is_err() {
            if self.stop.is_some() {
                self.stop();
                let _ = (&mut self.task).await;
            } else {
                self.task.abort();
                let _ = (&mut self.task).await;
            }
        }
        assert!(result.is_ok(), "{} listener owners did not complete", self.name);
        result.unwrap().unwrap()
    }
}

#[tokio::test(start_paused = true)]
async fn t3_completed_owner_cannot_discharge_another_live_owners_barrier() {
    let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = second.local_addr().unwrap();
    let mut completed = Completion::new("completed peer", async move { drop(first) });
    let retained = Completion::new("retained peer", async move { drop(second) });
    completed.stop();
    completed.assert_closed().await;
    let _connection = tokio::net::TcpStream::connect(address).await.unwrap();
    let failure = tokio::spawn(retained.assert_closed()).await.unwrap_err();
    assert!(failure.is_panic());
    let message = failure.into_panic().downcast::<String>().unwrap();
    assert!(message.contains("retained peer listener owners did not complete"));
}
