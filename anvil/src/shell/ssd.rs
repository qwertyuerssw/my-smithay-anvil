use std::cell::{Cell, RefCell, RefMut};
use std::rc::Rc;
use std::sync::Once;

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::LogicalPosition;
use slint::ComponentHandle;

use super::{WindowElement, WindowHeader};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::desktop::WindowSurface;
use smithay::input::Seat;
use smithay::utils::{Buffer, Logical, Point, Rectangle, Serial, Size};
use smithay::wayland::shell::xdg::XdgShellHandler;

use crate::{state::Backend, AnvilState};

thread_local! {
    static NEXT_SLINT_WINDOW: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) };
}

struct AnvilSlintPlatform;
impl slint::platform::Platform for AnvilSlintPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        let window = NEXT_SLINT_WINDOW.with(|w| w.borrow_mut().take())
            .unwrap_or_else(|| MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer));
        Ok(window)
    }
}

static INIT_SLINT: Once = Once::new();

pub const HEADER_BAR_HEIGHT: i32 = 32;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum HeaderAction {
    #[default]
    None,
    Close,
    Maximize,
}

pub struct HeaderBar {
    pub width: u32,
    pub pointer_loc: Option<Point<f64, Logical>>,
    pub ui: WindowHeader,
    pub slint_window: Rc<MinimalSoftwareWindow>,
    pub buffer: Option<MemoryRenderBuffer>,
    action: Rc<Cell<HeaderAction>>,
}

impl HeaderBar {
    pub fn new() -> Self {
        INIT_SLINT.call_once(|| {
            let _ = slint::platform::set_platform(Box::new(AnvilSlintPlatform));
        });

        let slint_window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        NEXT_SLINT_WINDOW.with(|w| *w.borrow_mut() = Some(slint_window.clone()));

        let ui = WindowHeader::new().expect("Failed to initialize Slint HeaderBar");
        let action = Rc::new(Cell::new(HeaderAction::None));

        // Привязываем колбеки нажатий кнопок из Slint:
        let a1 = action.clone();
        ui.on_close_clicked(move || a1.set(HeaderAction::Close));

        let a2 = action.clone();
        ui.on_maximize_clicked(move || a2.set(HeaderAction::Maximize));

        Self {
            width: 0,
            pointer_loc: None,
            ui,
            slint_window,
            buffer: None,
            action,
        }
    }

    pub fn pointer_enter(&mut self, loc: Point<f64, Logical>) {
        self.pointer_loc = Some(loc);
        self.slint_window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(loc.x as f32, loc.y as f32),
        });
    }

    pub fn pointer_leave(&mut self) {
        self.pointer_loc = None;
        self.slint_window.dispatch_event(WindowEvent::PointerExited);
    }

    pub fn clicked<BackendData: Backend>(
        &mut self,
        seat: &Seat<AnvilState<BackendData>>,
        state: &mut AnvilState<BackendData>,
        window: &WindowElement,
        serial: Serial,
    ) {
        if let Some(loc) = self.pointer_loc {
            let pos = LogicalPosition::new(loc.x as f32, loc.y as f32);
            
            // Сбрасываем действие перед отправкой клика в Slint
            self.action.set(HeaderAction::None);

            // Отправляем события клика мыши в движок Slint:
            self.slint_window.dispatch_event(WindowEvent::PointerPressed {
                position: pos,
                button: PointerEventButton::Left,
            });
            self.slint_window.dispatch_event(WindowEvent::PointerReleased {
                position: pos,
                button: PointerEventButton::Left,
            });

            // Выполняем то действие, которое стриггерил Slint:
            match self.action.take() {
                HeaderAction::Close => {
                    match window.0.underlying_surface() {
                        WindowSurface::Wayland(w) => w.send_close(),
                        #[cfg(feature = "xwayland")]
                        WindowSurface::X11(w) => {
                            let _ = w.close();
                        }
                    }
                }
                HeaderAction::Maximize => {
                    match window.0.underlying_surface() {
                        WindowSurface::Wayland(w) => state.maximize_request(w.clone()),
                        #[cfg(feature = "xwayland")]
                        WindowSurface::X11(w) => {
                            let surface = w.clone();
                            state
                                .handle
                                .insert_idle(move |data| data.maximize_request_x11(&surface));
                        }
                    }
                }
                HeaderAction::None => {
                    // Клик по пустой области шапки
                    match window.0.underlying_surface() {
                        WindowSurface::Wayland(w) => {
                            let seat = seat.clone();
                            let toplevel = w.clone();
                            state.handle.insert_idle(move |data| {
                                data.move_request_xdg(&toplevel, &seat, serial);
                            });
                        }
                        #[cfg(feature = "xwayland")]
                        WindowSurface::X11(w) => {
                            let window = w.clone();
                            state.handle.insert_idle(move |data| {
                                data.move_request_x11(&window);
                            });
                        }
                    }
                }
            }
        }
    }

    pub fn touch_down<BackendData: Backend>(
        &mut self,
        _seat: &Seat<AnvilState<BackendData>>,
        _state: &mut AnvilState<BackendData>,
        _window: &WindowElement,
        _serial: Serial,
    ) {}

    pub fn touch_up<BackendData: Backend>(
        &mut self,
        _seat: &Seat<AnvilState<BackendData>>,
        _state: &mut AnvilState<BackendData>,
        _window: &WindowElement,
    ) {}

    pub fn redraw(&mut self, width: u32) {
        if width == 0 {
            return;
        }

        let size_changed = self.width != width || self.buffer.is_none();

        if size_changed {
            self.width = width;
            self.ui.set_header_width(width as f32);
            self.slint_window.set_size(slint::PhysicalSize::new(width, HEADER_BAR_HEIGHT as u32));
            self.slint_window.request_redraw();

            let size: Size<i32, Buffer> = Size::from((width as i32, HEADER_BAR_HEIGHT));
            let mem_buffer = MemoryRenderBuffer::new(
                Fourcc::Abgr8888,
                size,
                1,
                smithay::utils::Transform::Normal,
                None,
            );
            self.buffer = Some(mem_buffer);
        }

        if let Some(mem_buffer) = self.buffer.as_mut() {
            let size: Size<i32, Buffer> = Size::from((self.width as i32, HEADER_BAR_HEIGHT));

            let _ = mem_buffer.render().draw(|slice| {
                let pixel_slice: &mut [PremultipliedRgbaColor] = unsafe {
                    std::slice::from_raw_parts_mut(
                        slice.as_mut_ptr() as *mut PremultipliedRgbaColor,
                        (self.width as usize) * (HEADER_BAR_HEIGHT as usize),
                    )
                };

                let drew = self.slint_window.draw_if_needed(|renderer| {
                    renderer.render(pixel_slice, self.width as usize);
                });

                if drew {
                    Result::<_, ()>::Ok(vec![Rectangle::from_loc_and_size(Point::default(), size)])
                } else {
                    Result::<_, ()>::Ok(vec![])
                }
            });
        }
    }
}

impl Default for HeaderBar {
    fn default() -> Self {
        Self::new()
    }
}

pub struct WindowState {
    pub is_ssd: bool,
    pub header_bar: HeaderBar,
}

impl WindowElement {
    pub fn decoration_state(&self) -> RefMut<'_, WindowState> {
        self.user_data().insert_if_missing(|| {
            RefCell::new(WindowState {
                is_ssd: false,
                header_bar: HeaderBar::new(),
            })
        });

        self.user_data()
            .get::<RefCell<WindowState>>()
            .unwrap()
            .borrow_mut()
    }

    pub fn set_ssd(&self, ssd: bool) {
        self.decoration_state().is_ssd = ssd;
    }
}
