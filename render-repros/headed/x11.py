"""Small X11/XTest driver for the native reproduction, using ctypes only."""
import ctypes as ffi
import time


class ClientData(ffi.Union):
    _fields_ = [('longs', ffi.c_long * 5), ('bytes', ffi.c_char * 20)]


class ClientMessage(ffi.Structure):
    _fields_ = [('type', ffi.c_int), ('serial', ffi.c_ulong), ('send_event', ffi.c_int),
                ('display', ffi.c_void_p), ('window', ffi.c_ulong), ('message_type', ffi.c_ulong),
                ('format', ffi.c_int), ('data', ClientData)]


class Event(ffi.Union):
    _fields_ = [('client', ClientMessage), ('padding', ffi.c_long * 24)]


class Desktop:
    def __init__(self):
        self.xlib = ffi.CDLL('libX11.so.6')
        self.xtest = ffi.CDLL('libXtst.so.6')
        pointer, window = ffi.c_void_p, ffi.c_ulong
        reference = ffi.POINTER
        signatures = {
            'XOpenDisplay': ([ffi.c_char_p], pointer),
            'XDefaultRootWindow': ([pointer], window),
            'XQueryTree': ([pointer, window, reference(window), reference(window), reference(reference(window)), reference(ffi.c_uint)], ffi.c_int),
            'XFetchName': ([pointer, window, reference(pointer)], ffi.c_int),
            'XFree': ([pointer], ffi.c_int),
            'XFlush': ([pointer], ffi.c_int),
            'XSetInputFocus': ([pointer, window, ffi.c_int, ffi.c_ulong], ffi.c_int),
            'XRaiseWindow': ([pointer, window], ffi.c_int),
            'XGetGeometry': ([pointer, window, reference(window), reference(ffi.c_int), reference(ffi.c_int), reference(ffi.c_uint), reference(ffi.c_uint), reference(ffi.c_uint), reference(ffi.c_uint)], ffi.c_int),
            'XTranslateCoordinates': ([pointer, window, window, ffi.c_int, ffi.c_int, reference(ffi.c_int), reference(ffi.c_int), reference(window)], ffi.c_int),
            'XStringToKeysym': ([ffi.c_char_p], ffi.c_ulong),
            'XKeysymToKeycode': ([pointer, ffi.c_ulong], ffi.c_uint),
            'XGetImage': ([pointer, window, ffi.c_int, ffi.c_int, ffi.c_uint, ffi.c_uint, ffi.c_ulong, ffi.c_int], pointer),
            'XGetPixel': ([pointer, ffi.c_int, ffi.c_int], ffi.c_ulong),
            'XDestroyImage': ([pointer], ffi.c_int),
            'XResizeWindow': ([pointer, window, ffi.c_uint, ffi.c_uint], ffi.c_int),
            'XInternAtom': ([pointer, ffi.c_char_p, ffi.c_int], ffi.c_ulong),
            'XSendEvent': ([pointer, window, ffi.c_int, ffi.c_long, reference(Event)], ffi.c_int),
        }
        for name, (arguments, result) in signatures.items():
            function = getattr(self.xlib, name)
            function.argtypes, function.restype = arguments, result
        for name, arguments in {
            'XTestFakeMotionEvent': [pointer, ffi.c_int, ffi.c_int, ffi.c_int, ffi.c_ulong],
            'XTestFakeButtonEvent': [pointer, ffi.c_uint, ffi.c_int, ffi.c_ulong],
            'XTestFakeKeyEvent': [pointer, ffi.c_uint, ffi.c_int, ffi.c_ulong],
        }.items():
            function = getattr(self.xtest, name)
            function.argtypes, function.restype = arguments, ffi.c_int
        self.display = self.xlib.XOpenDisplay(None)
        assert self.display, 'An X11 display is required for the native input reproduction'
        self.root = self.xlib.XDefaultRootWindow(self.display)

    def children(self, window):
        root, parent = ffi.c_ulong(), ffi.c_ulong()
        children, count = ffi.POINTER(ffi.c_ulong)(), ffi.c_uint()
        assert self.xlib.XQueryTree(self.display, window, ffi.byref(root), ffi.byref(parent), ffi.byref(children), ffi.byref(count))
        result = [children[index] for index in range(count.value)]
        if children:
            self.xlib.XFree(children)
        return result

    def find(self, process_id):
        pending = [(self.root, 0)]
        while pending:
            window, depth = pending.pop()
            name = ffi.c_void_p()
            if self.xlib.XFetchName(self.display, window, ffi.byref(name)) and name:
                title = ffi.string_at(name).decode('utf-8', 'replace')
                self.xlib.XFree(name)
                if title.startswith(f'Obscura [{process_id}]'):
                    return window
            if depth < 3:
                pending.extend((child, depth + 1) for child in self.children(window))
        return None

    def geometry(self, window):
        root, child = ffi.c_ulong(), ffi.c_ulong()
        left, top, x, y = ffi.c_int(), ffi.c_int(), ffi.c_int(), ffi.c_int()
        width, height, border, depth = (ffi.c_uint() for _ in range(4))
        assert self.xlib.XGetGeometry(self.display, window, ffi.byref(root), ffi.byref(x), ffi.byref(y), ffi.byref(width), ffi.byref(height), ffi.byref(border), ffi.byref(depth))
        assert self.xlib.XTranslateCoordinates(self.display, window, self.root, 0, 0, ffi.byref(left), ffi.byref(top), ffi.byref(child))
        return left.value, top.value, width.value, height.value

    def click(self, window, point):
        left, top, _, _ = self.geometry(window)
        self.xlib.XRaiseWindow(self.display, window)
        self.xlib.XSetInputFocus(self.display, window, 1, 0)
        self.xtest.XTestFakeMotionEvent(self.display, -1, left + round(point[0]), top + round(point[1]), 0)
        self.xtest.XTestFakeButtonEvent(self.display, 1, 1, 0)
        self.xtest.XTestFakeButtonEvent(self.display, 1, 0, 0)
        self.xlib.XFlush(self.display)

    def key(self, name, is_pressed):
        symbol = self.xlib.XStringToKeysym(name.encode())
        code = self.xlib.XKeysymToKeycode(self.display, symbol)
        assert code, name
        self.xtest.XTestFakeKeyEvent(self.display, code, int(is_pressed), 0)
        self.xlib.XFlush(self.display)

    def type_text(self, text):
        for character in text:
            if character.isupper():
                self.key('Shift_L', True)
            self.key(character, True)
            self.key(character, False)
            if character.isupper():
                self.key('Shift_L', False)
            time.sleep(0.02)

    def scroll(self):
        self.xtest.XTestFakeButtonEvent(self.display, 5, 1, 0)
        self.xtest.XTestFakeButtonEvent(self.display, 5, 0, 0)
        self.xlib.XFlush(self.display)

    def pixel(self, window, point):
        image = self.xlib.XGetImage(self.display, window, round(point[0]), round(point[1]), 1, 1, ffi.c_ulong(-1), 2)
        assert image, 'Native window pixels unavailable'
        result = self.xlib.XGetPixel(image, 0, 0) & 0xffffff
        self.xlib.XDestroyImage(image)
        return result

    def resize(self, window, width, height):
        self.xlib.XResizeWindow(self.display, window, width, height)
        self.xlib.XFlush(self.display)

    def close_window(self, window):
        event = Event()
        event.client.type, event.client.display, event.client.window = 33, self.display, window
        event.client.message_type = self.xlib.XInternAtom(self.display, b'WM_PROTOCOLS', 0)
        event.client.format = 32
        event.client.data.longs[0] = self.xlib.XInternAtom(self.display, b'WM_DELETE_WINDOW', 0)
        self.xlib.XSendEvent(self.display, window, 0, 0, ffi.byref(event))
        self.xlib.XFlush(self.display)
