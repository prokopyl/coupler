use crate::{Parent, Rect, WindowHandler, WindowOptions};

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::error::Error;
use std::marker::PhantomData;
use std::rc::Rc;
use std::{ffi, fmt, os, ptr};

use raw_window_handle::{unix::XlibHandle, HasRawWindowHandle, RawWindowHandle};
use xcb_sys as xcb;

unsafe fn intern_atom(
    connection: *mut xcb::xcb_connection_t,
    name: &[u8],
) -> xcb::xcb_intern_atom_cookie_t {
    xcb::xcb_intern_atom(connection, 1, name.len() as u16, name.as_ptr() as *const os::raw::c_char)
}

unsafe fn intern_atom_reply(
    connection: *mut xcb::xcb_connection_t,
    cookie: xcb::xcb_intern_atom_cookie_t,
) -> xcb::xcb_atom_t {
    let reply = xcb::xcb_intern_atom_reply(connection, cookie, ptr::null_mut());
    if reply.is_null() {
        return xcb::XCB_NONE;
    }
    let atom = (*reply).atom;
    libc::free(reply as *mut ffi::c_void);
    atom
}

#[derive(Debug)]
pub enum ApplicationError {
    ConnectionFailed(i32),
    GetEvent(i32),
    Close(Vec<WindowError>),
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for ApplicationError {}

#[derive(Clone)]
pub struct Application {
    inner: Rc<ApplicationInner>,
}

struct ApplicationInner {
    open: Cell<bool>,
    running: Cell<usize>,
    connection: *mut xcb::xcb_connection_t,
    screen: *mut xcb::xcb_screen_t,
    wm_protocols: xcb::xcb_atom_t,
    wm_delete_window: xcb::xcb_atom_t,
    windows: RefCell<HashMap<u32, crate::Window>>,
}

impl Application {
    pub fn open() -> Result<Application, ApplicationError> {
        unsafe {
            let mut default_screen_index = 0;
            let connection = xcb::xcb_connect(ptr::null(), &mut default_screen_index);

            let error = xcb::xcb_connection_has_error(connection);
            if error != 0 {
                xcb::xcb_disconnect(connection);
                return Err(ApplicationError::ConnectionFailed(error));
            }

            let setup = xcb::xcb_get_setup(connection);
            let mut roots_iter = xcb::xcb_setup_roots_iterator(setup);
            for _ in 0..default_screen_index {
                xcb::xcb_screen_next(&mut roots_iter);
            }
            let screen = roots_iter.data;

            let wm_protocols_cookie = intern_atom(connection, b"WM_PROTOCOLS");
            let wm_delete_window_cookie = intern_atom(connection, b"WM_DELETE_WINDOW");

            let wm_protocols = intern_atom_reply(connection, wm_protocols_cookie);
            let wm_delete_window = intern_atom_reply(connection, wm_delete_window_cookie);

            Ok(Application {
                inner: Rc::new(ApplicationInner {
                    open: Cell::new(true),
                    running: Cell::new(0),
                    connection,
                    screen,
                    wm_protocols,
                    wm_delete_window,
                    windows: RefCell::new(HashMap::new()),
                }),
            })
        }
    }

    pub fn close(&self) -> Result<(), ApplicationError> {
        unsafe {
            if self.inner.open.get() {
                self.inner.open.set(false);

                let mut window_errors = Vec::new();
                for (_, window) in self.inner.windows.take() {
                    if let Err(error) = window.window.close() {
                        window_errors.push(error);
                    }
                }

                xcb::xcb_disconnect(self.inner.connection);

                if !window_errors.is_empty() {
                    return Err(ApplicationError::Close(window_errors));
                }
            }

            Ok(())
        }
    }

    pub fn start(&self) -> Result<(), ApplicationError> {
        unsafe {
            if self.inner.open.get() {
                let depth = self.inner.running.get();
                self.inner.running.set(depth + 1);

                while self.inner.open.get() && self.inner.running.get() > depth {
                    let event = xcb::xcb_wait_for_event(self.inner.connection);

                    if event.is_null() {
                        let error = xcb::xcb_connection_has_error(self.inner.connection);
                        return Err(ApplicationError::GetEvent(error));
                    }

                    self.handle_event(event);
                }
            }

            Ok(())
        }
    }

    pub fn stop(&self) {
        unsafe {
            self.inner.running.set(self.inner.running.get().saturating_sub(1));
        }
    }

    pub fn poll(&self) {
        unsafe {
            while self.inner.open.get() {
                let event = xcb::xcb_poll_for_event(self.inner.connection);

                if event.is_null() {
                    break;
                }

                self.handle_event(event);
            }
        }
    }

    unsafe fn handle_event(&self, event: *mut xcb::xcb_generic_event_t) {
        match ((*event).response_type & !0x80) as u32 {
            xcb::XCB_EXPOSE => {
                let event = &*(event as *mut xcb_sys::xcb_expose_event_t);
                if let Some(window) = self.inner.windows.borrow().get(&event.window) {
                    window.window.state.expose_rects.borrow_mut().push(xcb::xcb_rectangle_t {
                        x: event.x as i16,
                        y: event.y as i16,
                        width: event.width,
                        height: event.height,
                    });

                    if event.count == 0 {
                        let rects = window.window.state.expose_rects.take();
                        xcb::xcb_set_clip_rectangles(
                            self.inner.connection,
                            xcb::XCB_CLIP_ORDERING_UNSORTED as u8,
                            window.window.state.gcontext_id,
                            0,
                            0,
                            rects.len() as u32,
                            rects.as_ptr(),
                        );

                        window.window.state.handler.display(window);

                        xcb::xcb_set_clip_rectangles(
                            self.inner.connection,
                            xcb::XCB_CLIP_ORDERING_UNSORTED as u8,
                            window.window.state.gcontext_id,
                            0,
                            0,
                            0,
                            ptr::null(),
                        );
                    }
                }
            }
            xcb::XCB_CLIENT_MESSAGE => {
                let event = &*(event as *mut xcb_sys::xcb_client_message_event_t);
                if event.data.data32[0] == self.inner.wm_delete_window {
                    let window = self.inner.windows.borrow().get(&event.window).cloned();
                    if let Some(window) = window {
                        window.window.state.handler.request_close(&window);
                    }
                }
            }
            _ => {}
        }

        libc::free(event as *mut ffi::c_void);
    }

    pub fn file_descriptor(&self) -> Option<std::os::raw::c_int> {
        unsafe {
            if self.inner.open.get() {
                Some(xcb::xcb_get_file_descriptor(self.inner.connection))
            } else {
                None
            }
        }
    }
}

#[derive(Debug)]
pub enum WindowError {
    ApplicationClosed,
    WindowClosed,
    WindowCreation(u8),
    MapWindow(u8),
    InvalidWindowHandle,
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for WindowError {}

#[derive(Clone)]
pub struct Window {
    state: Rc<WindowState>,
}

struct WindowState {
    open: Cell<bool>,
    window_id: xcb::xcb_window_t,
    gcontext_id: xcb::xcb_gcontext_t,
    expose_rects: RefCell<Vec<xcb::xcb_rectangle_t>>,
    application: crate::Application,
    handler: Box<dyn WindowHandler>,
}

impl Window {
    pub fn open(
        application: &crate::Application,
        options: WindowOptions,
    ) -> Result<crate::Window, WindowError> {
        unsafe {
            if !application.application.inner.open.get() {
                return Err(WindowError::ApplicationClosed);
            }

            let parent_id = if let Parent::Parent(parent) = options.parent {
                let parent_id = match parent.raw_window_handle() {
                    RawWindowHandle::Xcb(handle) => handle.window,
                    RawWindowHandle::Xlib(handle) => handle.window as u32,
                    _ => {
                        return Err(WindowError::InvalidWindowHandle);
                    }
                };

                if parent_id == 0 {
                    return Err(WindowError::InvalidWindowHandle);
                }

                parent_id
            } else {
                (*application.application.inner.screen).root
            };

            let window_id = xcb::xcb_generate_id(application.application.inner.connection);
            let value_mask = xcb::XCB_CW_EVENT_MASK;
            let value_list = &[xcb::XCB_EVENT_MASK_EXPOSURE];
            let cookie = xcb::xcb_create_window_checked(
                application.application.inner.connection,
                xcb::XCB_COPY_FROM_PARENT as u8,
                window_id,
                parent_id,
                options.rect.x as i16,
                options.rect.y as i16,
                options.rect.w as u16,
                options.rect.h as u16,
                0,
                xcb::XCB_WINDOW_CLASS_INPUT_OUTPUT as u16,
                (*application.application.inner.screen).root_visual,
                value_mask,
                value_list.as_ptr() as *const ffi::c_void,
            );

            let error = xcb::xcb_request_check(application.application.inner.connection, cookie);
            if !error.is_null() {
                let error_code = (*error).error_code;
                libc::free(error as *mut ffi::c_void);
                return Err(WindowError::WindowCreation(error_code));
            }

            let gcontext_id = xcb::xcb_generate_id(application.application.inner.connection);
            xcb::xcb_create_gc_checked(
                application.application.inner.connection,
                gcontext_id,
                window_id,
                0,
                ptr::null(),
            );

            let atoms = &[application.application.inner.wm_delete_window];
            xcb::xcb_icccm_set_wm_protocols(
                application.application.inner.connection,
                window_id,
                application.application.inner.wm_protocols,
                atoms.len() as u32,
                atoms.as_ptr() as *mut xcb::xcb_atom_t,
            );

            let title =
                ffi::CString::new(options.title).unwrap_or_else(|_| ffi::CString::default());
            xcb::xcb_change_property(
                application.application.inner.connection,
                xcb::XCB_PROP_MODE_REPLACE as u8,
                window_id,
                xcb::XCB_ATOM_WM_NAME,
                xcb::XCB_ATOM_STRING,
                8,
                title.as_bytes().len() as u32,
                title.as_ptr() as *const ffi::c_void,
            );

            let window = crate::Window {
                window: Window {
                    state: Rc::new(WindowState {
                        open: Cell::new(true),
                        window_id,
                        gcontext_id,
                        expose_rects: RefCell::new(Vec::new()),
                        application: application.clone(),
                        handler: options.handler,
                    }),
                },
                phantom: PhantomData,
            };

            application.application.inner.windows.borrow_mut().insert(window_id, window.clone());

            window.window.state.handler.create(&window);

            let cookie =
                xcb::xcb_map_window_checked(application.application.inner.connection, window_id);

            let error = xcb::xcb_request_check(application.application.inner.connection, cookie);
            if !error.is_null() {
                let error_code = (*error).error_code;
                libc::free(error as *mut ffi::c_void);
                return Err(WindowError::MapWindow(error_code));
            }

            xcb::xcb_flush(application.application.inner.connection);

            Ok(window)
        }
    }

    pub fn request_display(&self) {}

    pub fn request_display_rect(&self, rect: Rect) {}

    pub fn update_contents(&self, framebuffer: &[u32], width: usize, height: usize) {}

    pub fn close(&self) -> Result<(), WindowError> {
        unsafe {
            if self.state.open.get() {
                self.state.open.set(false);

                let application = &self.state.application;
                application.application.inner.windows.borrow_mut().remove(&self.state.window_id);

                let window = crate::Window { window: self.clone(), phantom: PhantomData };
                window.window.state.handler.destroy(&window);

                xcb::xcb_free_gc(
                    self.state.application.application.inner.connection,
                    self.state.gcontext_id,
                );

                let cookie = xcb::xcb_destroy_window_checked(
                    self.state.application.application.inner.connection,
                    self.state.window_id,
                );
                let error = xcb::xcb_request_check(
                    self.state.application.application.inner.connection,
                    cookie,
                );

                if !error.is_null() {
                    libc::free(error as *mut ffi::c_void);
                    return Err(WindowError::WindowClosed);
                }
            }

            Ok(())
        }
    }

    pub fn application(&self) -> &crate::Application {
        &self.state.application
    }
}

unsafe impl HasRawWindowHandle for Window {
    fn raw_window_handle(&self) -> RawWindowHandle {
        RawWindowHandle::Xlib(XlibHandle { ..XlibHandle::empty() })
    }
}
