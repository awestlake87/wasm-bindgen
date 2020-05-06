use js_sys::Promise;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use wasm_bindgen::{JsCast, prelude::*};

struct QueueStateInner {
    high_priority_tasks: VecDeque<Rc<crate::task::Task>>,
    tasks: VecDeque<Rc<crate::task::Task>>,

    /// The number of times a task can be popped off the queue before unblocking the event loop
    coop_budget: u32
}

struct QueueState {
    // The queue of Tasks which will be run in order. In practice this is all the
    // synchronous work of futures, and each `Task` represents calling `poll` on
    // a future "at the right time"
    inner: RefCell<QueueStateInner>,

    // This flag indicates whether we're currently executing inside of
    // `run_all` or have scheduled `run_all` to run in the future. This is
    // used to ensure that it's only scheduled once.
    is_spinning: Cell<bool>,
}

impl QueueState {
    fn run_all(&self) {
        debug_assert!(self.is_spinning.get());

        // Runs all Tasks until empty. This blocks the event loop if a Future is
        // stuck in an infinite loop, so we may want to yield back to the main
        // event loop occasionally. For now though greedy execution should get
        // the job done.
        loop {
            let task = match self.inner.borrow_mut().high_priority_tasks.pop_front() {
                Some(task) => task,
                None => break,
            };
            task.run();
        }

        let mut i = 0;
        let coop_budget = self.inner.borrow_mut().coop_budget;

        loop {
            if i > coop_budget {
                break;
            }
            
            let task = match self.inner.borrow_mut().tasks.pop_front() {
                Some(task) => task,
                None => break,
            };
            task.run();

            i += 1;
        }

        if i > coop_budget && !self.inner.borrow_mut().tasks.is_empty() {
            // our budget was exceeded before the queue was exhausted
            QUEUE.with(|queue| {
                queue.schedule_queue_update();
            });
        } else { 
            // All of the Tasks have been run, so it's now possible to schedule the
            // next tick again
            self.is_spinning.set(false);
        }
    }
}

pub(crate) struct Queue {
    state: Rc<QueueState>,
    promise: Promise,
    closure: Closure<dyn FnMut(JsValue)>,
}

impl Queue {
    pub(crate) fn push_high_priority_task(&self, task: Rc<crate::task::Task>) {
        self.state.inner.borrow_mut().high_priority_tasks.push_back(task);

        // If we're already inside the `run_all` loop then that'll pick up the
        // task we just enqueued. If we're not in `run_all`, though, then we need
        // to schedule a microtask.
        //
        // Note that we currently use a promise and a closure to do this, but
        // eventually we should probably use something like `queueMicrotask`:
        // https://developer.mozilla.org/en-US/docs/Web/API/WindowOrWorkerGlobalScope/queueMicrotask
        if !self.state.is_spinning.replace(true) {
            self.spawn_queue_microtask();
        }
    }

    pub(crate) fn push_task(&self, task: Rc<crate::task::Task>) {
        self.state.inner.borrow_mut().tasks.push_back(task);

        // If we're already inside the `run_all` loop then that'll pick up the
        // task we just enqueued. If we're not in `run_all`, though, then we need
        // to schedule a microtask.
        //
        // Note that we currently use a promise and a closure to do this, but
        // eventually we should probably use something like `queueMicrotask`:
        // https://developer.mozilla.org/en-US/docs/Web/API/WindowOrWorkerGlobalScope/queueMicrotask
        if !self.state.is_spinning.replace(true) {
            self.spawn_queue_microtask();
        }
    }

    fn spawn_queue_microtask(&self) {
        let _ = self.promise.then(&self.closure);
    }

    fn schedule_queue_update(&self) {
        web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(self.closure.as_ref().unchecked_ref(), 0).unwrap_throw();
    }

    pub(crate) fn set_coop_budget(&self, budget: u32) {
        self.state.inner.borrow_mut().coop_budget = budget;
    }
}

impl Queue {
    fn new() -> Self {
        let state = Rc::new(QueueState {
            is_spinning: Cell::new(false),
            inner: RefCell::new(QueueStateInner {
                high_priority_tasks: VecDeque::new(),
                tasks: VecDeque::new(),
                coop_budget: u32::MAX // effectively unlimited by default
            }),
        });

        Self {
            promise: Promise::resolve(&JsValue::undefined()),

            closure: {
                let state = Rc::clone(&state);

                // This closure will only be called on the next microtask event
                // tick
                Closure::wrap(Box::new(move |_| state.run_all()))
            },

            state,
        }
    }
}

thread_local! {
    pub(crate) static QUEUE: Queue = Queue::new();
}
