use window::{Application, MouseButton, Point, Rect, Window, WindowHandler, WindowOptions};

struct Handler;

impl WindowHandler for Handler {
    fn create(&self, _window: &Window) {
        println!("create");
    }

    fn frame(&self, _window: &Window) {
        println!("frame");
    }

    fn display(&self, window: &Window) {
        println!("display");
        window.update_contents(&[0xFFFF00FF; 1920 * 1920], 1920, 1920);
    }

    fn mouse_move(&self, _window: &Window, position: Point) {
        println!("mouse move: {:?}", position);
    }

    fn mouse_down(&self, _window: &Window, button: MouseButton) -> bool {
        println!("mouse down: {:?}", button);
        false
    }

    fn mouse_up(&self, _window: &Window, button: MouseButton) -> bool {
        println!("mouse up: {:?}", button);
        false
    }

    fn scroll(&self, _window: &Window, dx: f64, dy: f64) -> bool {
        println!("scroll: {}, {}", dx, dy);
        false
    }

    fn request_close(&self, window: &Window) {
        window.close().unwrap();
    }

    fn destroy(&self, window: &Window) {
        println!("destroy");
        window.application().stop();
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        println!("drop");
    }
}

fn main() {
    let app = Application::new().unwrap();

    Window::open(
        &app,
        WindowOptions {
            title: "window".to_string(),
            rect: Rect { x: 0.0, y: 0.0, width: 500.0, height: 500.0 },
            handler: Box::new(Handler),
            ..WindowOptions::default()
        },
    )
    .unwrap();

    app.start().unwrap();
}
