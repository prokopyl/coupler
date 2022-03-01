use std::marker::PhantomData;
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{Ordering, AtomicPtr};
use std::sync::{Arc, Mutex, Condvar};
use std::thread::{self, JoinHandle, Thread};
use std::any::Any;

use crossbeam_channel::Sender;

type Task = Box<dyn FnOnce() + Send>;

struct Context {
    thread: Thread,
    task_count: Mutex<usize>,
    zero_tasks: Condvar,
    panic: AtomicPtr<Box<dyn Any + Send>>,
}

pub struct ThreadPool {
    handles: Vec<JoinHandle<()>>,
    sender: Option<Sender<Task>>,
    context: Arc<Context>,
}

impl ThreadPool {
    pub fn with_threads(num_threads: usize) -> ThreadPool {
        assert!(num_threads != 0);

        let (sender, receiver) = crossbeam_channel::unbounded::<Task>();

        let context = Arc::new(Context {
            thread: thread::current(),
            task_count: Mutex::new(0),
            zero_tasks: Condvar::new(),
            panic: AtomicPtr::new(ptr::null_mut()),
        });

        let mut handles = Vec::with_capacity(num_threads);
        for _ in 0..num_threads {
            let receiver = receiver.clone();
            let context = context.clone();

            let handle = thread::spawn(move || {
                while let Ok(task) = receiver.recv() {
                    let result = panic::catch_unwind(AssertUnwindSafe(|| {
                        task();
                    }));

                    {
                        let mut task_count = context.task_count.lock().unwrap();
                        *task_count -= 1;
                        if *task_count == 0 {
                            context.zero_tasks.notify_one();
                        }
                    }

                    if let Err(err) = result {
                        unsafe {
                            let ptr = Box::into_raw(Box::new(err));

                            let result = context.panic.compare_exchange(
                                ptr::null_mut(),
                                ptr,
                                Ordering::Release,
                                Ordering::Relaxed,
                            );

                            if result.is_err() {
                                drop(Box::from_raw(ptr));
                            }
                        }
                    }
                }
            });

            handles.push(handle);
        }

        ThreadPool { handles, sender: Some(sender), context }
    }

    pub fn scope<F>(&mut self, f: F)
    where
        F: FnOnce(&Scope),
    {
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            f(&Scope { pool: self, phantom: PhantomData });
        }));

        {
            let mut task_count = self.context.task_count.lock().unwrap();
            while *task_count != 0 {
                task_count = self.context.zero_tasks.wait(task_count).unwrap();
            }
        }

        unsafe {
            let panic = self.context.panic.swap(ptr::null_mut(), Ordering::Relaxed);
            if !panic.is_null() {
                panic::resume_unwind(*Box::from_raw(panic));
            }
        }

        if let Err(err) = result {
            panic::resume_unwind(err);
        }
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        drop(self.sender.take());

        for handle in self.handles.drain(0..self.handles.len()) {
            handle.join().unwrap();
        }
    }
}

pub struct Scope<'p, 's> {
    pool: &'p ThreadPool,
    phantom: PhantomData<fn(&'s ())>,
}

impl<'p, 's> Scope<'p, 's> {
    pub fn spawn<F>(&self, task: F)
    where
        F: FnOnce() + Send + 's,
    {
        let task: Box<dyn FnOnce() + Send> = Box::new(task);
        let task: Box<dyn FnOnce() + Send + 'static> = unsafe { mem::transmute(task) };

        *self.pool.context.task_count.lock().unwrap() += 1;
        self.pool.sender.as_ref().unwrap().send(task).unwrap();
    }
}
