use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use deno_core::{new_submission_queue, SubmissionQueue, SubmissionQueueFutures};

#[derive(Default)]
struct TestFutures(VecDeque<ReentrantFuture>);

struct ReentrantFuture {
    queue: Rc<RefCell<Option<SubmissionQueue<TestFutures>>>>,
    spawn_nested: bool,
    value: u8,
}

impl Future for ReentrantFuture {
    type Output = u8;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.spawn_nested {
            self.spawn_nested = false;
            self.queue
                .borrow()
                .as_ref()
                .unwrap()
                .spawn(ReentrantFuture {
                    queue: self.queue.clone(),
                    spawn_nested: false,
                    value: 2,
                });
        }
        Poll::Ready(self.value)
    }
}

impl SubmissionQueueFutures for TestFutures {
    type Future = ReentrantFuture;
    type Output = u8;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn spawn(&mut self, future: Self::Future) {
        self.0.push_back(future);
    }

    fn poll_next_unpin(&mut self, cx: &mut Context) -> Poll<Self::Output> {
        let mut future = self.0.pop_front().unwrap();
        Pin::new(&mut future).poll(cx)
    }
}

#[test]
fn polling_future_can_submit_another_future() {
    let (queue, mut results) = new_submission_queue::<TestFutures>();
    let queue_slot = Rc::new(RefCell::new(None));
    queue.spawn(ReentrantFuture {
        queue: queue_slot.clone(),
        spawn_nested: true,
        value: 1,
    });
    *queue_slot.borrow_mut() = Some(queue);

    let mut cx = Context::from_waker(Waker::noop());
    assert_eq!(results.poll_next_unpin(&mut cx), Poll::Ready(1));
    assert_eq!(results.poll_next_unpin(&mut cx), Poll::Ready(2));
}
