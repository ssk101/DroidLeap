// tablet-edge: push the pointer against an outer edge of the Windows desktop to
// hand the mouse over to an Android tablet via `scrcpy --otg`. Pause toggles.
//
// Settings are read from tablet-edge.ini next to the exe and reloaded when it
// changes; the tray icon opens a live log monitor and a settings dialog.

#define WIN32_LEAN_AND_MEAN
#define UNICODE
#define _UNICODE
#include <windows.h>
#include <cderr.h>
#include <commdlg.h>
#include <shellapi.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

#include "resource.h"

#define APP_NAME L"tablet-edge"
#define APP_TITLE L"Tablet Edge"
#define INI_SECTION L"tablet-edge"

#define HOTKEY_ID 1
#define TIMER_CONFIG 1
#define CONFIG_POLL_MS 2000
#define COOLDOWN_MS 3000
#define STOP_GRACE_MS 2000

#define WM_TRAY (WM_APP + 1)
// lParam is a malloc'd wchar_t * log line, freed by the receiver.
#define WM_LOG (WM_APP + 2)
#define MENU_TOGGLE 100
#define MENU_SETTINGS 101
#define MENU_EXIT 102
#define MENU_MONITOR 103

#define MAX_MONITORS 16
#define MAX_PUSH_EVENTS 1024
#define LOG_ROTATE_BYTES (1024 * 1024)
// Characters of recent log kept in memory for the log monitor.
#define HISTORY_MAX (128 * 1024)

enum side { SIDE_LEFT, SIDE_RIGHT, SIDE_TOP, SIDE_BOTTOM };
static const wchar_t *const SIDE_NAMES[] = {L"left", L"right", L"top", L"bottom"};
static const wchar_t *const SIDE_LABELS[] = {L"Left", L"Right", L"Top", L"Bottom"};

static struct {
    enum side side;
    // Raw mouse counts, not pixels: unaffected by pointer acceleration.
    int threshold;
    DWORD timeout_ms;
    int corner_margin;
    wchar_t serial[64];
    wchar_t scrcpy[MAX_PATH];
} cfg;

static HINSTANCE g_inst;
static wchar_t g_ini[MAX_PATH];
static FILETIME g_ini_mtime;
static wchar_t g_log_path[MAX_PATH];
static HANDLE g_log = INVALID_HANDLE_VALUE;
static CRITICAL_SECTION g_log_lock;
static HWND g_hwnd;
static HWND g_monitor;
static HFONT g_log_font;
static bool g_settings_open;
static HICON g_tray_icon;
static UINT g_taskbar_created;

static wchar_t g_history[HISTORY_MAX + 1];
static size_t g_history_len;

static RECT g_monitors[MAX_MONITORS];
static int g_monitor_count;
static RECT g_desktop;

static struct { DWORD t; int d; } g_push[MAX_PUSH_EVENTS];
static int g_push_head, g_push_count, g_push_sum;
static DWORD g_last_trigger;

static PROCESS_INFORMATION g_proc;
static HWINEVENTHOOK g_show_hook;

static void update_status(void);

// UI thread only.
static void
history_append(const wchar_t *line) {
    size_t n = wcslen(line);
    if (n > HISTORY_MAX) {
        return;
    }
    if (g_history_len + n > HISTORY_MAX) {
        // Drop the oldest half, cut at a line boundary.
        wchar_t *nl = wcschr(g_history + g_history_len / 2, L'\n');
        size_t drop = nl ? (size_t) (nl - g_history) + 1 : g_history_len;
        memmove(g_history, g_history + drop, (g_history_len - drop) * sizeof(wchar_t));
        g_history_len -= drop;
    }
    memcpy(g_history + g_history_len, line, n * sizeof(wchar_t));
    g_history_len += n;
    g_history[g_history_len] = 0;
}

// Thread-safe: called from the UI thread and from the scrcpy output reader.
static void
log_write(const wchar_t *source, const wchar_t *msg) {
    SYSTEMTIME st;
    GetLocalTime(&st);
    wchar_t line[1200];
    int n = _snwprintf_s(line, ARRAYSIZE(line) - 2, _TRUNCATE,
                         L"%04d-%02d-%02d %02d:%02d:%02d %ls: %ls", st.wYear, st.wMonth,
                         st.wDay, st.wHour, st.wMinute, st.wSecond, source, msg);
    if (n < 0) {
        n = (int) wcslen(line);
    }
    line[n++] = L'\r';
    line[n++] = L'\n';
    line[n] = 0;

    char utf8[3 * ARRAYSIZE(line)];
    int bytes = WideCharToMultiByte(CP_UTF8, 0, line, n, utf8, sizeof(utf8), NULL, NULL);
    EnterCriticalSection(&g_log_lock);
    if (g_log != INVALID_HANDLE_VALUE && bytes > 0) {
        DWORD written;
        WriteFile(g_log, utf8, bytes, &written, NULL);
    }
    LeaveCriticalSection(&g_log_lock);

    if (!g_hwnd) {
        history_append(line); // still starting up, on the UI thread
        return;
    }
    wchar_t *copy = _wcsdup(line);
    if (copy && !PostMessageW(g_hwnd, WM_LOG, 0, (LPARAM) copy)) {
        free(copy);
    }
}

static void
log_msg(const wchar_t *fmt, ...) {
    wchar_t msg[1024];
    va_list ap;
    va_start(ap, fmt);
    _vsnwprintf_s(msg, ARRAYSIZE(msg), _TRUNCATE, fmt, ap);
    va_end(ap);
    log_write(APP_NAME, msg);
}

static void
open_log(void) {
    DWORD len = GetTempPathW(MAX_PATH, g_log_path);
    if (!len || len + 20 > MAX_PATH) {
        g_log_path[0] = 0;
        return;
    }
    wcscat(g_log_path, APP_NAME L".log");

    WIN32_FILE_ATTRIBUTE_DATA a;
    if (GetFileAttributesExW(g_log_path, GetFileExInfoStandard, &a)
            && (a.nFileSizeHigh || a.nFileSizeLow > LOG_ROTATE_BYTES)) {
        wchar_t old[MAX_PATH + 4];
        _snwprintf_s(old, ARRAYSIZE(old), _TRUNCATE, L"%ls.1", g_log_path);
        MoveFileExW(g_log_path, old, MOVEFILE_REPLACE_EXISTING);
    }

    g_log = CreateFileW(g_log_path, FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE,
                        NULL, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
}

static bool
ini_mtime(FILETIME *out) {
    WIN32_FILE_ATTRIBUTE_DATA a;
    if (!GetFileAttributesExW(g_ini, GetFileExInfoStandard, &a)) {
        return false;
    }
    *out = a.ftLastWriteTime;
    return true;
}

static void
load_config(void) {
    wchar_t side[16];
    GetPrivateProfileStringW(INI_SECTION, L"side", L"left", side, 16, g_ini);
    cfg.side = SIDE_LEFT;
    bool known = false;
    for (int i = 0; i < 4; ++i) {
        if (!_wcsicmp(side, SIDE_NAMES[i])) {
            cfg.side = i;
            known = true;
        }
    }
    if (!known) {
        log_msg(L"unknown side '%ls', using left", side);
    }

    cfg.threshold = GetPrivateProfileIntW(INI_SECTION, L"threshold", 400, g_ini);
    cfg.timeout_ms = GetPrivateProfileIntW(INI_SECTION, L"timeout_ms", 1000, g_ini);
    cfg.corner_margin = GetPrivateProfileIntW(INI_SECTION, L"corner_margin", 0, g_ini);
    GetPrivateProfileStringW(INI_SECTION, L"serial", L"", cfg.serial,
                             ARRAYSIZE(cfg.serial), g_ini);
    GetPrivateProfileStringW(INI_SECTION, L"scrcpy", L"scrcpy.exe", cfg.scrcpy,
                             ARRAYSIZE(cfg.scrcpy), g_ini);

    g_push_head = g_push_count = g_push_sum = 0;
    log_msg(L"config: side=%ls threshold=%d timeout_ms=%lu corner_margin=%d serial=%ls scrcpy=%ls",
            SIDE_NAMES[cfg.side], cfg.threshold, cfg.timeout_ms, cfg.corner_margin,
            cfg.serial[0] ? cfg.serial : L"(any)", cfg.scrcpy);
    update_status();
}

static BOOL CALLBACK
add_monitor(HMONITOR mon, HDC dc, LPRECT rect, LPARAM data) {
    (void) dc;
    (void) rect;
    (void) data;
    MONITORINFO mi = {sizeof(mi)};
    if (g_monitor_count < MAX_MONITORS && GetMonitorInfoW(mon, &mi)) {
        g_monitors[g_monitor_count++] = mi.rcMonitor;
    }
    return TRUE;
}

static void
load_monitors(void) {
    g_monitor_count = 0;
    EnumDisplayMonitors(NULL, NULL, add_monitor, 0);
    SetRectEmpty(&g_desktop);
    for (int i = 0; i < g_monitor_count; ++i) {
        UnionRect(&g_desktop, &g_desktop, &g_monitors[i]);
    }
    log_msg(L"%d monitor(s), desktop %ld,%ld - %ld,%ld", g_monitor_count,
            g_desktop.left, g_desktop.top, g_desktop.right, g_desktop.bottom);
}

// True if p sits on a monitor edge that is also the outer edge of the whole
// desktop on the configured side, outside the corner dead zones.
static bool
at_outer_edge(POINT p) {
    int m = cfg.corner_margin;
    for (int i = 0; i < g_monitor_count; ++i) {
        const RECT *r = &g_monitors[i];
        switch (cfg.side) {
            case SIDE_LEFT:
                if (r->left == g_desktop.left && p.x == r->left
                        && p.y >= r->top + m && p.y < r->bottom - m) {
                    return true;
                }
                break;
            case SIDE_RIGHT:
                if (r->right == g_desktop.right && p.x == r->right - 1
                        && p.y >= r->top + m && p.y < r->bottom - m) {
                    return true;
                }
                break;
            case SIDE_TOP:
                if (r->top == g_desktop.top && p.y == r->top
                        && p.x >= r->left + m && p.x < r->right - m) {
                    return true;
                }
                break;
            case SIDE_BOTTOM:
                if (r->bottom == g_desktop.bottom && p.y == r->bottom - 1
                        && p.x >= r->left + m && p.x < r->right - m) {
                    return true;
                }
                break;
        }
    }
    return false;
}

// Something (a game, or scrcpy itself) has confined the cursor: stay out of it.
static bool
cursor_is_clipped(void) {
    RECT clip;
    return GetClipCursor(&clip) && !EqualRect(&clip, &g_desktop);
}

static void
reset_pressure(void) {
    g_push_head = g_push_count = g_push_sum = 0;
}

// Sliding-window sum of pushes over timeout_ms, like GNOME's PressureBarrier.
static int
add_pressure(DWORD now, int amount) {
    while (g_push_count) {
        int tail = (g_push_head - g_push_count + MAX_PUSH_EVENTS) % MAX_PUSH_EVENTS;
        if (now - g_push[tail].t <= cfg.timeout_ms && g_push_count < MAX_PUSH_EVENTS) {
            break;
        }
        g_push_sum -= g_push[tail].d;
        --g_push_count;
    }
    g_push[g_push_head].t = now;
    g_push[g_push_head].d = amount;
    g_push_head = (g_push_head + 1) % MAX_PUSH_EVENTS;
    ++g_push_count;
    g_push_sum += amount;
    return g_push_sum;
}

static bool
scrcpy_running(void) {
    if (!g_proc.hProcess) {
        return false;
    }
    if (WaitForSingleObject(g_proc.hProcess, 0) == WAIT_TIMEOUT) {
        return true;
    }
    DWORD code = 0;
    GetExitCodeProcess(g_proc.hProcess, &code);
    log_msg(L"scrcpy exited with code %lu", code);
    CloseHandle(g_proc.hProcess);
    CloseHandle(g_proc.hThread);
    ZeroMemory(&g_proc, sizeof(g_proc));
    update_status();
    return false;
}

static void
force_foreground(HWND hwnd) {
    // Windows only lets the foreground process hand focus over; borrow its input
    // queue so SetForegroundWindow is honoured even though we run in background.
    HWND fg = GetForegroundWindow();
    DWORD fg_tid = fg ? GetWindowThreadProcessId(fg, NULL) : 0;
    DWORD me = GetCurrentThreadId();
    bool attached = fg_tid && fg_tid != me && AttachThreadInput(me, fg_tid, TRUE);
    SetForegroundWindow(hwnd);
    BringWindowToTop(hwnd);
    if (attached) {
        AttachThreadInput(me, fg_tid, FALSE);
    }

    if (GetForegroundWindow() != hwnd) {
        // Last resort: a synthetic Alt tap lifts the foreground lock.
        INPUT in[2] = {0};
        in[0].type = in[1].type = INPUT_KEYBOARD;
        in[0].ki.wVk = in[1].ki.wVk = VK_MENU;
        in[1].ki.dwFlags = KEYEVENTF_KEYUP;
        SendInput(2, in, sizeof(INPUT));
        SetForegroundWindow(hwnd);
    }
    log_msg(L"scrcpy window %ls foreground", GetForegroundWindow() == hwnd ? L"is" : L"is NOT");
}

static void CALLBACK
on_window_shown(HWINEVENTHOOK hook, DWORD event, HWND hwnd, LONG obj, LONG child,
                DWORD thread, DWORD time) {
    (void) hook;
    (void) event;
    (void) thread;
    (void) time;
    if (obj != OBJID_WINDOW || child != CHILDID_SELF || GetAncestor(hwnd, GA_ROOT) != hwnd) {
        return;
    }
    wchar_t cls[64] = L"";
    GetClassNameW(hwnd, cls, ARRAYSIZE(cls));
    if (!wcsncmp(cls, L"IME", 3) || !wcsncmp(cls, L"MSCTFIME", 8)) {
        return;
    }
    log_msg(L"scrcpy window shown (class %ls)", cls);

    // Alpha 1 rather than 0 keeps the window hit-testable for mouse buttons.
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, GetWindowLongPtrW(hwnd, GWL_EXSTYLE) | WS_EX_LAYERED);
    SetLayeredWindowAttributes(hwnd, 0, 1, LWA_ALPHA);
    force_foreground(hwnd);

    UnhookWinEvent(g_show_hook);
    g_show_hook = NULL;
}

static void
log_scrcpy_line(const char *line, size_t len) {
    while (len && (line[len - 1] == '\r' || line[len - 1] == '\n')) {
        --len;
    }
    if (!len) {
        return;
    }
    wchar_t wide[1024];
    int n = MultiByteToWideChar(CP_UTF8, 0, line, (int) len, wide, ARRAYSIZE(wide) - 1);
    wide[n > 0 ? n : 0] = 0;
    log_write(L"scrcpy", wide);
}

// Background thread: forwards scrcpy's stdout/stderr into the log line by line.
static DWORD WINAPI
read_scrcpy_output(LPVOID param) {
    HANDLE pipe = param;
    char buf[4096];
    size_t len = 0;
    DWORD n;
    while (ReadFile(pipe, buf + len, (DWORD) (sizeof(buf) - len), &n, NULL) && n) {
        len += n;
        size_t start = 0;
        for (size_t i = 0; i < len; ++i) {
            if (buf[i] == '\n') {
                log_scrcpy_line(buf + start, i - start);
                start = i + 1;
            }
        }
        if (start == 0 && len == sizeof(buf)) {
            log_scrcpy_line(buf, len); // overlong line
            start = len;
        }
        memmove(buf, buf + start, len - start);
        len -= start;
    }
    log_scrcpy_line(buf, len);
    CloseHandle(pipe);
    return 0;
}

static void
start_scrcpy(void) {
    wchar_t cmd[MAX_PATH + 128];
    if (cfg.serial[0]) {
        swprintf(cmd, ARRAYSIZE(cmd), L"\"%ls\" --otg -s %ls", cfg.scrcpy, cfg.serial);
    } else {
        swprintf(cmd, ARRAYSIZE(cmd), L"\"%ls\" --otg", cfg.scrcpy);
    }

    // Only the pipe's write end is inheritable, so it is all scrcpy gets.
    SECURITY_ATTRIBUTES sa = {sizeof(sa), NULL, TRUE};
    HANDLE out_read = NULL, out_write = NULL;
    if (CreatePipe(&out_read, &out_write, &sa, 0)) {
        SetHandleInformation(out_read, HANDLE_FLAG_INHERIT, 0);
    } else {
        log_msg(L"could not create output pipe (error %lu), scrcpy output not logged",
                GetLastError());
        out_read = out_write = NULL;
    }

    STARTUPINFOW si = {sizeof(si)};
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdOutput = si.hStdError = out_write;

    BOOL ok = CreateProcessW(NULL, cmd, NULL, NULL, out_write != NULL, CREATE_NO_WINDOW,
                             NULL, NULL, &si, &g_proc);
    DWORD err = GetLastError();
    if (out_write) {
        CloseHandle(out_write);
    }
    if (!ok) {
        log_msg(L"could not start %ls (error %lu)", cmd, err);
        if (out_read) {
            CloseHandle(out_read);
        }
        ZeroMemory(&g_proc, sizeof(g_proc));
        return;
    }
    log_msg(L"started: %ls (pid %lu)", cmd, g_proc.dwProcessId);

    if (out_read) {
        HANDLE reader = CreateThread(NULL, 0, read_scrcpy_output, out_read, 0, NULL);
        if (reader) {
            CloseHandle(reader);
        } else {
            CloseHandle(out_read);
        }
    }

    if (g_show_hook) {
        UnhookWinEvent(g_show_hook);
    }
    g_show_hook = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, NULL, on_window_shown,
                                  g_proc.dwProcessId, 0, WINEVENT_OUTOFCONTEXT);
    update_status();
}

static BOOL CALLBACK
close_if_scrcpy(HWND hwnd, LPARAM pid) {
    DWORD owner;
    GetWindowThreadProcessId(hwnd, &owner);
    if (owner == (DWORD) pid) {
        PostMessageW(hwnd, WM_CLOSE, 0, 0);
    }
    return TRUE;
}

static void
stop_scrcpy(void) {
    // WM_CLOSE lets scrcpy release the AOA device cleanly.
    EnumWindows(close_if_scrcpy, g_proc.dwProcessId);
    if (WaitForSingleObject(g_proc.hProcess, STOP_GRACE_MS) == WAIT_TIMEOUT) {
        log_msg(L"scrcpy did not quit, terminating");
        TerminateProcess(g_proc.hProcess, 1);
    }
    scrcpy_running();
}

static void
toggle_scrcpy(void) {
    if (scrcpy_running()) {
        stop_scrcpy();
    } else {
        start_scrcpy();
    }
}

static void
on_raw_mouse(const RAWMOUSE *m) {
    if (m->usFlags & MOUSE_MOVE_ABSOLUTE) {
        return; // tablets, remote desktop: no meaningful "push"
    }
    int push;
    switch (cfg.side) {
        case SIDE_LEFT: push = -m->lLastX; break;
        case SIDE_RIGHT: push = m->lLastX; break;
        case SIDE_TOP: push = -m->lLastY; break;
        default: push = m->lLastY; break;
    }

    POINT p;
    if (!GetCursorPos(&p) || !at_outer_edge(p)) {
        reset_pressure();
        return;
    }
    if (push <= 0 || cursor_is_clipped()) {
        return;
    }

    DWORD now = GetTickCount();
    if (add_pressure(now, push) < cfg.threshold) {
        return;
    }
    reset_pressure();

    if (now - g_last_trigger < COOLDOWN_MS || scrcpy_running()) {
        return;
    }
    g_last_trigger = now;
    log_msg(L"edge triggered at %ld,%ld", p.x, p.y);
    start_scrcpy();
}

// ---- Log monitor -----------------------------------------------------------

static void
update_status(void) {
    if (!g_monitor) {
        return;
    }
    // Uses g_proc directly: scrcpy_running() calls back into here.
    bool active = g_proc.hProcess != NULL;
    wchar_t status[256];
    if (active) {
        _snwprintf_s(status, ARRAYSIZE(status), _TRUNCATE,
                     L"Tablet active (scrcpy pid %lu). Press Pause or click Release to get "
                     L"the mouse back.", g_proc.dwProcessId);
    } else {
        _snwprintf_s(status, ARRAYSIZE(status), _TRUNCATE,
                     L"Idle. Push against the %ls edge of the desktop, or press Pause, to "
                     L"take over the tablet.", SIDE_NAMES[cfg.side]);
    }
    SetDlgItemTextW(g_monitor, IDC_STATUS, status);
    SetDlgItemTextW(g_monitor, IDC_TOGGLE, active ? L"Release tablet" : L"Take over tablet");
}

static void
set_log_font(HWND dlg, UINT dpi) {
    HFONT font = CreateFontW(-MulDiv(9, dpi, 72), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
                             DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                             CLEARTYPE_QUALITY, FIXED_PITCH | FF_MODERN, L"Consolas");
    if (!font) {
        return;
    }
    SendDlgItemMessageW(dlg, IDC_LOG, WM_SETFONT, (WPARAM) font, TRUE);
    if (g_log_font) {
        DeleteObject(g_log_font);
    }
    g_log_font = font;
}

static void
scroll_log_to_end(HWND edit) {
    int len = GetWindowTextLengthW(edit);
    SendMessageW(edit, EM_SETSEL, len, len);
    SendMessageW(edit, EM_SCROLLCARET, 0, 0);
}

static void
monitor_append(const wchar_t *line) {
    HWND edit = GetDlgItem(g_monitor, IDC_LOG);
    int len = GetWindowTextLengthW(edit);
    if (len > 2 * HISTORY_MAX) {
        // The in-memory history is already trimmed; resync instead of growing.
        SetWindowTextW(edit, g_history);
        scroll_log_to_end(edit);
        return;
    }
    SendMessageW(edit, EM_SETSEL, len, len);
    SendMessageW(edit, EM_REPLACESEL, FALSE, (LPARAM) line);
}

static void
layout_monitor(HWND dlg) {
    RECT client;
    GetClientRect(dlg, &client);
    RECT margin = {7, 4, 0, 0}; // dialog units
    MapDialogRect(dlg, &margin);

    HWND status = GetDlgItem(dlg, IDC_STATUS);
    RECT r;
    GetWindowRect(status, &r);
    MapWindowPoints(NULL, dlg, (POINT *) &r, 2);
    MoveWindow(status, r.left, r.top, client.right - r.left - margin.left, r.bottom - r.top,
               TRUE);

    GetWindowRect(GetDlgItem(dlg, IDC_TOGGLE), &r);
    MapWindowPoints(NULL, dlg, (POINT *) &r, 2);
    int top = r.bottom + margin.top;
    MoveWindow(GetDlgItem(dlg, IDC_LOG), margin.left, top, client.right - 2 * margin.left,
               client.bottom - top - margin.left, TRUE);
}

static void open_settings(HWND parent);

static INT_PTR CALLBACK
monitor_proc(HWND dlg, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
        case WM_INITDIALOG: {
            g_monitor = dlg;
            SendMessageW(dlg, WM_SETICON, ICON_BIG,
                         (LPARAM) LoadIconW(g_inst, MAKEINTRESOURCEW(IDI_APP)));
            SendMessageW(dlg, WM_SETICON, ICON_SMALL, (LPARAM) g_tray_icon);
            HWND edit = GetDlgItem(dlg, IDC_LOG);
            SendMessageW(edit, EM_SETLIMITTEXT, 0, 0);
            set_log_font(dlg, GetDpiForWindow(dlg));
            SetWindowTextW(edit, g_history);
            scroll_log_to_end(edit);
            layout_monitor(dlg);
            update_status();
            return TRUE;
        }
        case WM_SIZE:
            layout_monitor(dlg);
            return TRUE;
        case WM_DPICHANGED:
            set_log_font(dlg, HIWORD(wp));
            return FALSE; // the dialog manager rescales everything else
        case WM_GETMINMAXINFO: {
            UINT dpi = GetDpiForWindow(dlg);
            MINMAXINFO *mmi = (MINMAXINFO *) lp;
            mmi->ptMinTrackSize.x = MulDiv(520, dpi, 96);
            mmi->ptMinTrackSize.y = MulDiv(260, dpi, 96);
            return TRUE;
        }
        case WM_COMMAND:
            switch (LOWORD(wp)) {
                case IDC_TOGGLE:
                    toggle_scrcpy();
                    return TRUE;
                case IDC_SETTINGS:
                    open_settings(dlg);
                    return TRUE;
                case IDC_OPENLOG:
                    if (g_log_path[0]) {
                        ShellExecuteW(dlg, L"open", L"notepad.exe", g_log_path, NULL,
                                      SW_SHOWNORMAL);
                    }
                    return TRUE;
                case IDC_CLEAR:
                    g_history_len = 0;
                    g_history[0] = 0;
                    SetDlgItemTextW(dlg, IDC_LOG, L"");
                    return TRUE;
                case IDCANCEL: // close button or Esc
                    DestroyWindow(dlg);
                    return TRUE;
            }
            break;
        case WM_NCDESTROY: // after the edit control is gone, so its font can go too
            g_monitor = NULL;
            if (g_log_font) {
                DeleteObject(g_log_font);
                g_log_font = NULL;
            }
            return TRUE;
    }
    return FALSE;
}

static void
show_monitor(void) {
    if (!g_monitor
            && !CreateDialogParamW(g_inst, MAKEINTRESOURCEW(IDD_MONITOR), NULL, monitor_proc, 0)) {
        log_msg(L"could not open the log monitor (error %lu)", GetLastError());
        return;
    }
    ShowWindow(g_monitor, IsIconic(g_monitor) ? SW_RESTORE : SW_SHOW);
    SetForegroundWindow(g_monitor);
}

// ---- Settings dialog -------------------------------------------------------

static bool
read_number(HWND dlg, int id, int min, int max, const wchar_t *name, int *out) {
    BOOL ok;
    UINT value = GetDlgItemInt(dlg, id, &ok, FALSE);
    if (!ok || value < (UINT) min || value > (UINT) max) {
        wchar_t msg[160];
        _snwprintf_s(msg, ARRAYSIZE(msg), _TRUNCATE, L"%ls must be a number from %d to %d.",
                     name, min, max);
        MessageBoxW(dlg, msg, APP_TITLE, MB_OK | MB_ICONWARNING);
        SetFocus(GetDlgItem(dlg, id));
        SendDlgItemMessageW(dlg, id, EM_SETSEL, 0, -1);
        return false;
    }
    *out = (int) value;
    return true;
}

static bool
save_settings(HWND dlg) {
    int threshold, timeout, margin;
    if (!read_number(dlg, IDC_THRESHOLD, 1, 100000, L"Push threshold", &threshold)
            || !read_number(dlg, IDC_TIMEOUT, 50, 10000, L"Push window", &timeout)
            || !read_number(dlg, IDC_MARGIN, 0, 2000, L"Corner dead zone", &margin)) {
        return false;
    }

    wchar_t scrcpy[ARRAYSIZE(cfg.scrcpy)];
    GetDlgItemTextW(dlg, IDC_SCRCPY, scrcpy, ARRAYSIZE(scrcpy));
    if (!scrcpy[0]) {
        MessageBoxW(dlg, L"Please enter the path to scrcpy.exe.", APP_TITLE,
                    MB_OK | MB_ICONWARNING);
        SetFocus(GetDlgItem(dlg, IDC_SCRCPY));
        return false;
    }
    wchar_t serial[ARRAYSIZE(cfg.serial)];
    GetDlgItemTextW(dlg, IDC_SERIAL, serial, ARRAYSIZE(serial));

    LRESULT side = SendDlgItemMessageW(dlg, IDC_SIDE, CB_GETCURSEL, 0, 0);
    if (side < SIDE_LEFT || side > SIDE_BOTTOM) {
        side = SIDE_LEFT;
    }

    wchar_t num[3][16];
    _snwprintf_s(num[0], 16, _TRUNCATE, L"%d", threshold);
    _snwprintf_s(num[1], 16, _TRUNCATE, L"%d", timeout);
    _snwprintf_s(num[2], 16, _TRUNCATE, L"%d", margin);

    // WritePrivateProfileString edits in place, keeping the file's comments.
    bool ok = WritePrivateProfileStringW(INI_SECTION, L"side", SIDE_NAMES[side], g_ini)
           && WritePrivateProfileStringW(INI_SECTION, L"threshold", num[0], g_ini)
           && WritePrivateProfileStringW(INI_SECTION, L"timeout_ms", num[1], g_ini)
           && WritePrivateProfileStringW(INI_SECTION, L"corner_margin", num[2], g_ini)
           && WritePrivateProfileStringW(INI_SECTION, L"serial", serial, g_ini)
           && WritePrivateProfileStringW(INI_SECTION, L"scrcpy", scrcpy, g_ini);
    if (!ok) {
        wchar_t msg[MAX_PATH + 64];
        _snwprintf_s(msg, ARRAYSIZE(msg), _TRUNCATE, L"Could not save settings to %ls (error %lu).",
                     g_ini, GetLastError());
        MessageBoxW(dlg, msg, APP_TITLE, MB_OK | MB_ICONERROR);
        return false;
    }
    WritePrivateProfileStringW(NULL, NULL, NULL, g_ini); // flush
    ini_mtime(&g_ini_mtime); // the file watcher need not reload it again
    log_msg(L"settings saved");
    load_config();
    return true;
}

static void
browse_scrcpy(HWND dlg) {
    wchar_t path[MAX_PATH];
    GetDlgItemTextW(dlg, IDC_SCRCPY, path, MAX_PATH);
    OPENFILENAMEW ofn = {sizeof(ofn)};
    ofn.hwndOwner = dlg;
    ofn.lpstrFilter = L"scrcpy.exe\0scrcpy.exe\0Programs (*.exe)\0*.exe\0";
    ofn.lpstrFile = path;
    ofn.nMaxFile = MAX_PATH;
    ofn.lpstrTitle = L"Locate scrcpy.exe";
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_HIDEREADONLY;
    bool picked = GetOpenFileNameW(&ofn);
    if (!picked && CommDlgExtendedError() == FNERR_INVALIDFILENAME) {
        path[0] = 0; // the current value is not a usable starting point
        picked = GetOpenFileNameW(&ofn);
    }
    if (picked) {
        SetDlgItemTextW(dlg, IDC_SCRCPY, path);
    }
}

static INT_PTR CALLBACK
settings_proc(HWND dlg, UINT msg, WPARAM wp, LPARAM lp) {
    (void) lp;
    switch (msg) {
        case WM_INITDIALOG: {
            HWND side = GetDlgItem(dlg, IDC_SIDE);
            for (int i = SIDE_LEFT; i <= SIDE_BOTTOM; ++i) {
                SendMessageW(side, CB_ADDSTRING, 0, (LPARAM) SIDE_LABELS[i]);
            }
            SendMessageW(side, CB_SETCURSEL, cfg.side, 0);
            SetDlgItemInt(dlg, IDC_THRESHOLD, cfg.threshold, FALSE);
            SetDlgItemInt(dlg, IDC_TIMEOUT, cfg.timeout_ms, FALSE);
            SetDlgItemInt(dlg, IDC_MARGIN, cfg.corner_margin, FALSE);
            SendDlgItemMessageW(dlg, IDC_SERIAL, EM_LIMITTEXT, ARRAYSIZE(cfg.serial) - 1, 0);
            SendDlgItemMessageW(dlg, IDC_SCRCPY, EM_LIMITTEXT, ARRAYSIZE(cfg.scrcpy) - 1, 0);
            SetDlgItemTextW(dlg, IDC_SERIAL, cfg.serial);
            SetDlgItemTextW(dlg, IDC_SCRCPY, cfg.scrcpy);
            SetForegroundWindow(dlg); // may be opened from the tray with no owner
            return TRUE;
        }
        case WM_COMMAND:
            switch (LOWORD(wp)) {
                case IDC_BROWSE:
                    browse_scrcpy(dlg);
                    return TRUE;
                case IDOK:
                    if (save_settings(dlg)) {
                        EndDialog(dlg, IDOK);
                    }
                    return TRUE;
                case IDCANCEL:
                    EndDialog(dlg, IDCANCEL);
                    return TRUE;
            }
            break;
    }
    return FALSE;
}

static void
open_settings(HWND parent) {
    if (g_settings_open) {
        return;
    }
    g_settings_open = true;
    DialogBoxParamW(g_inst, MAKEINTRESOURCEW(IDD_SETTINGS), parent, settings_proc, 0);
    g_settings_open = false;
}

// ---- Tray and main window --------------------------------------------------

static void
add_tray_icon(void) {
    NOTIFYICONDATAW nid = {sizeof(nid)};
    nid.hWnd = g_hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = g_tray_icon;
    swprintf(nid.szTip, ARRAYSIZE(nid.szTip), APP_TITLE L" (click for log, Pause toggles)");
    Shell_NotifyIconW(NIM_ADD, &nid);
}

static void
remove_tray_icon(void) {
    NOTIFYICONDATAW nid = {sizeof(nid)};
    nid.hWnd = g_hwnd;
    nid.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

static void
show_tray_menu(void) {
    HMENU menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, MENU_MONITOR, L"Show log monitor");
    AppendMenuW(menu, MF_STRING, MENU_SETTINGS, L"Settings...");
    AppendMenuW(menu, MF_STRING, MENU_TOGGLE,
                scrcpy_running() ? L"Release tablet\tPause" : L"Take over tablet\tPause");
    AppendMenuW(menu, MF_SEPARATOR, 0, NULL);
    AppendMenuW(menu, MF_STRING, MENU_EXIT, L"Exit");
    SetMenuDefaultItem(menu, MENU_MONITOR, FALSE);

    POINT p;
    GetCursorPos(&p);
    SetForegroundWindow(g_hwnd); // required for the menu to dismiss properly
    TrackPopupMenu(menu, TPM_RIGHTBUTTON, p.x, p.y, 0, g_hwnd, NULL);
    DestroyMenu(menu);
}

static LRESULT CALLBACK
wnd_proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
        case WM_INPUT: {
            RAWINPUT ri;
            UINT size = sizeof(ri);
            if (GetRawInputData((HRAWINPUT) lp, RID_INPUT, &ri, &size,
                                sizeof(RAWINPUTHEADER)) != (UINT) -1
                    && ri.header.dwType == RIM_TYPEMOUSE) {
                on_raw_mouse(&ri.data.mouse);
            }
            break;
        }
        case WM_HOTKEY:
            if (wp == HOTKEY_ID) {
                toggle_scrcpy();
            }
            return 0;
        case WM_LOG: {
            wchar_t *line = (wchar_t *) lp;
            history_append(line);
            if (g_monitor) {
                monitor_append(line);
            }
            free(line);
            return 0;
        }
        case WM_DISPLAYCHANGE:
            load_monitors();
            reset_pressure();
            break;
        case WM_TIMER:
            if (wp == TIMER_CONFIG) {
                FILETIME t;
                if (ini_mtime(&t) && CompareFileTime(&t, &g_ini_mtime)) {
                    g_ini_mtime = t;
                    load_config();
                }
                scrcpy_running(); // reap a scrcpy that quit on its own
            }
            return 0;
        case WM_TRAY:
            if (LOWORD(lp) == WM_LBUTTONUP) {
                show_monitor();
            } else if (LOWORD(lp) == WM_RBUTTONUP) {
                show_tray_menu();
            }
            return 0;
        case WM_COMMAND:
            switch (LOWORD(wp)) {
                case MENU_MONITOR:
                    show_monitor();
                    break;
                case MENU_SETTINGS:
                    open_settings(g_monitor);
                    break;
                case MENU_TOGGLE:
                    toggle_scrcpy();
                    break;
                case MENU_EXIT:
                    DestroyWindow(hwnd);
                    break;
            }
            return 0;
        case WM_DESTROY:
            if (g_monitor) {
                DestroyWindow(g_monitor);
            }
            remove_tray_icon();
            PostQuitMessage(0);
            return 0;
        default:
            if (msg == g_taskbar_created) {
                add_tray_icon(); // Explorer restarted
                return 0;
            }
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

int WINAPI
WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmdline, int show) {
    (void) prev;
    (void) cmdline;
    (void) show;

    HANDLE mutex = CreateMutexW(NULL, TRUE, L"Local\\" APP_NAME);
    if (GetLastError() == ERROR_ALREADY_EXISTS) {
        // Launching it again opens the running instance's log monitor.
        HWND other = FindWindowW(APP_NAME, NULL);
        if (other) {
            DWORD pid;
            GetWindowThreadProcessId(other, &pid);
            AllowSetForegroundWindow(pid);
            PostMessageW(other, WM_COMMAND, MENU_MONITOR, 0);
        }
        return 0;
    }

    // DPI awareness (per-monitor v2) comes from the embedded manifest.
    g_inst = inst;
    InitializeCriticalSection(&g_log_lock);
    open_log();

    GetModuleFileNameW(NULL, g_ini, MAX_PATH);
    wchar_t *slash = wcsrchr(g_ini, L'\\');
    wcscpy(slash ? slash + 1 : g_ini, APP_NAME L".ini");
    ini_mtime(&g_ini_mtime);
    load_config();
    load_monitors();

    WNDCLASSW wc = {0};
    wc.lpfnWndProc = wnd_proc;
    wc.hInstance = inst;
    wc.hIcon = LoadIconW(inst, MAKEINTRESOURCEW(IDI_APP));
    wc.lpszClassName = APP_NAME;
    RegisterClassW(&wc);

    // Pick the small-icon size for the current DPI from the multi-size .ico.
    g_tray_icon = LoadImageW(inst, MAKEINTRESOURCEW(IDI_APP), IMAGE_ICON,
                             GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON),
                             LR_DEFAULTCOLOR);
    if (!g_tray_icon) {
        g_tray_icon = LoadIconW(NULL, IDI_APPLICATION);
    }
    // A hidden top-level window (not message-only) so it also gets
    // WM_DISPLAYCHANGE and TaskbarCreated broadcasts.
    g_hwnd = CreateWindowExW(0, APP_NAME, APP_NAME, WS_OVERLAPPED, 0, 0, 0, 0,
                             NULL, NULL, inst, NULL);
    if (!g_hwnd) {
        log_msg(L"could not create window (error %lu)", GetLastError());
        return 1;
    }

    RAWINPUTDEVICE rid = {0x01, 0x02, RIDEV_INPUTSINK, g_hwnd}; // generic mouse
    if (!RegisterRawInputDevices(&rid, 1, sizeof(rid))) {
        log_msg(L"could not register raw input (error %lu)", GetLastError());
        return 1;
    }

    if (!RegisterHotKey(g_hwnd, HOTKEY_ID, MOD_NOREPEAT, VK_PAUSE)) {
        log_msg(L"could not register the Pause hotkey (error %lu), edge trigger still works",
                GetLastError());
    }

    g_taskbar_created = RegisterWindowMessageW(L"TaskbarCreated");
    add_tray_icon();
    SetTimer(g_hwnd, TIMER_CONFIG, CONFIG_POLL_MS, NULL);
    log_msg(L"running, settings: %ls, log: %ls", g_ini, g_log_path);

    MSG msg;
    while (GetMessageW(&msg, NULL, 0, 0) > 0) {
        if (g_monitor && IsDialogMessageW(g_monitor, &msg)) {
            continue; // Tab/Esc navigation in the modeless log monitor
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    if (scrcpy_running()) {
        stop_scrcpy();
    }
    ReleaseMutex(mutex);
    return 0;
}
