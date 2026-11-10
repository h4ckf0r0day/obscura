use std::ffi::{c_char, c_void};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use libloading::Library;

type Handle = *mut c_void;
type GetDisplay = unsafe extern "C" fn(Handle) -> Handle;
type GetPlatformDisplay = unsafe extern "C" fn(u32, Handle, *const i32) -> Handle;
type Initialize = unsafe extern "C" fn(Handle, *mut i32, *mut i32) -> u32;
type ChooseConfig = unsafe extern "C" fn(Handle, *const i32, *mut Handle, i32, *mut i32) -> u32;
type CreateContext = unsafe extern "C" fn(Handle, Handle, Handle, *const i32) -> Handle;
type CreateSurface = unsafe extern "C" fn(Handle, Handle, *const i32) -> Handle;
type MakeCurrent = unsafe extern "C" fn(Handle, Handle, Handle, Handle) -> u32;
type Destroy = unsafe extern "C" fn(Handle, Handle) -> u32;
type Terminate = unsafe extern "C" fn(Handle) -> u32;
type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *const c_void;

pub struct Display {
    display: usize,
    config: usize,
    create_context: CreateContext,
    create_surface: CreateSurface,
    make_current: MakeCurrent,
    destroy_context: Destroy,
    destroy_surface: Destroy,
    terminate: Terminate,
    get_proc_address: GetProcAddress,
    libraries: [Library; 2],
}

impl Display {
    pub fn shared() -> Result<Arc<Self>, String> {
        static DISPLAY: OnceLock<Mutex<Weak<Display>>> = OnceLock::new();
        let mut shared = DISPLAY.get_or_init(|| Mutex::new(Weak::new()))
            .lock().map_err(|_| "EGL display lock poisoned")?;
        if let Some(display) = shared.upgrade() {
            return Ok(display);
        }
        let display = Arc::new(unsafe { Self::load()? });
        *shared = Arc::downgrade(&display);
        Ok(display)
    }

    unsafe fn load() -> Result<Self, String> {
        let egl = Library::new("libEGL.so.1").map_err(|error| error.to_string())?;
        let gles = Library::new("libGLESv2.so.2").map_err(|error| error.to_string())?;
        macro_rules! symbol {
            ($name:literal, $kind:ty) => {
                *egl.get::<$kind>(concat!($name, "\0").as_bytes())
                    .map_err(|error| error.to_string())?
            };
        }
        let get_proc_address = symbol!("eglGetProcAddress", GetProcAddress);
        let platform = get_proc_address(c"eglGetPlatformDisplayEXT".as_ptr());
        let mut display = std::ptr::null_mut();
        if !platform.is_null() {
            let get_platform: GetPlatformDisplay = std::mem::transmute(platform);
            display = get_platform(0x31DD, std::ptr::null_mut(), std::ptr::null());
        }
        if display.is_null() {
            display = symbol!("eglGetDisplay", GetDisplay)(std::ptr::null_mut());
        }
        let initialize = symbol!("eglInitialize", Initialize);
        let mut major = 0;
        let mut minor = 0;
        if display.is_null() || initialize(display, &mut major, &mut minor) == 0 {
            return Err("No usable surfaceless EGL display".into());
        }
        let terminate = symbol!("eglTerminate", Terminate);
        let choose = symbol!("eglChooseConfig", ChooseConfig);
        let attributes = [0x3033, 1, 0x3040, 4, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, 0x3038];
        let mut config = std::ptr::null_mut();
        let mut count = 0;
        let bind_api = symbol!("eglBindAPI", unsafe extern "C" fn(u32) -> u32);
        if choose(display, attributes.as_ptr(), &mut config, 1, &mut count) == 0
            || count == 0 || bind_api(0x30A0) == 0
        {
            terminate(display);
            return Err("No EGL OpenGL ES 2 pbuffer configuration".into());
        }
        Ok(Self {
            display: display as usize,
            config: config as usize,
            create_context: symbol!("eglCreateContext", CreateContext),
            create_surface: symbol!("eglCreatePbufferSurface", CreateSurface),
            make_current: symbol!("eglMakeCurrent", MakeCurrent),
            destroy_context: symbol!("eglDestroyContext", Destroy),
            destroy_surface: symbol!("eglDestroySurface", Destroy),
            terminate,
            get_proc_address,
            libraries: [egl, gles],
        })
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        unsafe { (self.terminate)(self.display as Handle); }
    }
}

pub struct Surface {
    pub gl: glow::Context,
    context: usize,
    surface: usize,
    display: Arc<Display>,
}

impl Surface {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        let display = Display::shared()?;
        unsafe {
            let context = (display.create_context)(display.display as Handle,
                display.config as Handle, std::ptr::null_mut(), [0x3098, 2, 0x3038].as_ptr());
            if context.is_null() { return Err("EGL context creation failed".into()); }
            let surface = (display.create_surface)(display.display as Handle,
                display.config as Handle, [0x3057, width as i32, 0x3056, height as i32, 0x3038].as_ptr());
            if surface.is_null() {
                (display.destroy_context)(display.display as Handle, context);
                return Err("EGL pbuffer creation failed".into());
            }
            if (display.make_current)(display.display as Handle, surface, surface, context) == 0 {
                (display.destroy_surface)(display.display as Handle, surface);
                (display.destroy_context)(display.display as Handle, context);
                return Err("EGL make-current failed".into());
            }
            let gl = glow::Context::from_loader_function(|name| {
                let name = std::ffi::CString::new(name).expect("GL symbol");
                let address = (display.get_proc_address)(name.as_ptr());
                if !address.is_null() { return address; }
                display.libraries[1].get::<*const c_void>(name.as_bytes_with_nul())
                    .map(|symbol| *symbol).unwrap_or(std::ptr::null())
            });
            Ok(Self { display, context: context as usize, surface: surface as usize, gl })
        }
    }

    pub fn activate(&self) -> Result<(), String> {
        let result = unsafe { (self.display.make_current)(self.display.display as Handle,
            self.surface as Handle, self.surface as Handle, self.context as Handle) };
        if result == 0 { return Err("EGL context is unavailable".into()); }
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        unsafe {
            let surface = (self.display.create_surface)(self.display.display as Handle,
                self.display.config as Handle, [0x3057, width as i32, 0x3056, height as i32, 0x3038].as_ptr());
            if surface.is_null() { return Err("EGL resize failed".into()); }
            if (self.display.make_current)(self.display.display as Handle, surface, surface,
                self.context as Handle) == 0
            {
                (self.display.destroy_surface)(self.display.display as Handle, surface);
                return Err("EGL resized surface is unavailable".into());
            }
            (self.display.destroy_surface)(self.display.display as Handle, self.surface as Handle);
            self.surface = surface as usize;
        }
        Ok(())
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            (self.display.make_current)(self.display.display as Handle,
                std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            (self.display.destroy_surface)(self.display.display as Handle, self.surface as Handle);
            (self.display.destroy_context)(self.display.display as Handle, self.context as Handle);
        }
    }
}
