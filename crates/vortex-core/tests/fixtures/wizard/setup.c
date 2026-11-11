/* setup_lantern_hollow.exe: a click-through Win32 installer wizard for the fictional free game
 * "Lantern Hollow". Pages: Welcome -> License (accept) -> Destination -> Installing -> Finish.
 *
 * Everything is found by RELATIVE path from the current directory (license.txt, the two .bin
 * parts, manual\, readme.txt, redist\prereq_setup.exe), which is what Vortex's working-directory
 * contract must satisfy. The data archive (see lhpack.c) is split across
 * setup_lantern_hollow-1.bin and -2.bin; entries are checksummed (FNV-1a).
 *
 * Exit codes: 0 installed, 2 cancelled, 3 missing or corrupt .bin (named in a message box),
 * 4 other install error. LH_AUTOCLICK=1 makes the wizard press its own buttons (BM_CLICK on a
 * timer) through the same pages; LH_AUTOCLICK=cancel presses Cancel on the License page.
 * LH_TRACE=<file> logs each page shown and button pressed. */
#include "lh.h"

#define IDC_BACK 101
#define IDC_NEXT 102
#define IDC_CANCEL 103
#define IDC_ACCEPT 104
#define IDC_DEST 105
#define IDC_LAUNCH 106
#define WM_LH_PROGRESS (WM_APP + 1)
#define WM_LH_DONE (WM_APP + 2)
#define DEFAULT_DEST "C:\\Games\\Lantern Hollow"
#define TITLE "Lantern Hollow Setup"

enum { P_WELCOME, P_LICENSE, P_DEST, P_INSTALL, P_FINISH };
static const char *page_names[] = {"welcome", "license", "destination", "installing", "finish"};

static HWND wnd, title, body, license, accept_box, dest, progress, status, launch, back, next, cancel;
static int page = -1;
static char dest_dir[MAX_PATH];
static char fail_msg[512];
static HFONT bold;

static void button(const char *what) { trace("press %s %s", page_names[page], what); }

static void show(HWND h, int on) { ShowWindow(h, on ? SW_SHOW : SW_HIDE); }

static void set_page(int p) {
    static const char *titles[] = {
        "Welcome to the Lantern Hollow Setup Wizard",
        "License Agreement",
        "Select Destination Location",
        "Installing",
        "Completing the Lantern Hollow Setup Wizard",
    };
    static const char *bodies[] = {
        "This will install Lantern Hollow 1.0 on your computer.\r\n\r\n"
        "Lantern Hollow is a free game about keeping the lights on in a quiet valley.\r\n\r\n"
        "Click Next to continue, or Cancel to exit Setup.",
        "Please read the following License Agreement. You must accept the terms of this "
        "agreement before continuing with the installation.",
        "Setup will install Lantern Hollow into the following folder. To continue, click "
        "Install. To use a different folder, type its path.",
        "Please wait while Setup installs Lantern Hollow on your computer.",
        "Setup has finished installing Lantern Hollow on your computer.\r\n\r\n"
        "Click Finish to exit Setup.",
    };
    page = p;
    SetWindowTextA(title, titles[p]);
    SetWindowTextA(body, bodies[p]);
    show(license, p == P_LICENSE);
    show(accept_box, p == P_LICENSE);
    show(dest, p == P_DEST);
    show(progress, p == P_INSTALL);
    show(status, p == P_INSTALL);
    show(launch, p == P_FINISH);
    EnableWindow(back, p == P_LICENSE || p == P_DEST);
    EnableWindow(cancel, p != P_INSTALL && p != P_FINISH);
    EnableWindow(next, p != P_INSTALL &&
                           (p != P_LICENSE || SendMessageA(accept_box, BM_GETCHECK, 0, 0) == BST_CHECKED));
    SetWindowTextA(next, p == P_DEST ? "&Install" : p == P_FINISH ? "&Finish" : "&Next >");
    trace("page %s", page_names[p]);
}

/* ---- the install worker ---- */

static int progress_total, progress_done;

static void step(const char *what) {
    progress_done++;
    char *s = _strdup(what);
    PostMessageA(wnd, WM_LH_PROGRESS, progress_done * 1000 / (progress_total ? progress_total : 1),
                 (LPARAM)s);
}

static FILE *log_out; /* unins000.dat: every installed file, relative to the install dir */

static int write_file(const char *rel, const unsigned char *data, size_t n) {
    char p[MAX_PATH];
    snprintf(p, sizeof p, "%s\\%s", dest_dir, rel);
    char *slash = strrchr(p, '\\');
    *slash = 0;
    mkdirs(p);
    *slash = '\\';
    FILE *f = fopen(p, "wb");
    if (!f || fwrite(data, 1, n, f) != n) {
        if (f) fclose(f);
        return 0;
    }
    fclose(f);
    fprintf(log_out, "%s\n", rel);
    return 1;
}

static int copy_tree(const char *rel) {
    char pat[MAX_PATH];
    snprintf(pat, sizeof pat, "%s\\*", rel);
    WIN32_FIND_DATAA fd;
    HANDLE h = FindFirstFileA(pat, &fd);
    if (h == INVALID_HANDLE_VALUE) return 0;
    int ok = 1;
    do {
        if (!strcmp(fd.cFileName, ".") || !strcmp(fd.cFileName, "..")) continue;
        char sub[MAX_PATH];
        snprintf(sub, sizeof sub, "%s\\%s", rel, fd.cFileName);
        if (fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
            ok = ok && copy_tree(sub);
        } else {
            size_t n;
            unsigned char *b = read_all(sub, &n);
            ok = ok && b && write_file(sub, b, n);
            free(b);
            step(sub);
        }
    } while (FindNextFileA(h, &fd));
    FindClose(h);
    return ok;
}

static int fail(int code, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(fail_msg, sizeof fail_msg, fmt, ap);
    va_end(ap);
    return code;
}

static DWORD rd32(const unsigned char *p) { return p[0] | p[1] << 8 | p[2] << 16 | (DWORD)p[3] << 24; }

static int run_prereq(const char *args, int wait) {
    char cmd[MAX_PATH];
    snprintf(cmd, sizeof cmd, "\"redist\\prereq_setup.exe\" %s", args);
    STARTUPINFOA si = {sizeof si};
    PROCESS_INFORMATION pi;
    if (!CreateProcessA("redist\\prereq_setup.exe", cmd, NULL, NULL, FALSE,
                        wait ? 0 : DETACHED_PROCESS, NULL, NULL, &si, &pi))
        return -1;
    DWORD code = 0;
    if (wait) {
        WaitForSingleObject(pi.hProcess, INFINITE);
        GetExitCodeProcess(pi.hProcess, &code);
    }
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    return (int)code;
}

static int install(void) {
    static const char *parts[2] = {"setup_lantern_hollow-1.bin", "setup_lantern_hollow-2.bin"};
    size_t len[2];
    unsigned char *b[2];
    for (int i = 0; i < 2; i++) {
        b[i] = read_all(parts[i], &len[i]);
        if (!b[i])
            return fail(3, "Setup cannot find the file %s.\r\n\r\nPlace it next to "
                           "setup_lantern_hollow.exe and run Setup again.", parts[i]);
    }
    unsigned char *a = malloc(len[0] + len[1]);
    memcpy(a, b[0], len[0]);
    memcpy(a + len[0], b[1], len[1]);
    size_t total = len[0] + len[1], off = 12;
    if (total < 12 || memcmp(a, "LHPK", 4) || rd32(a + 4) != 1)
        return fail(3, "The file %s is not a Lantern Hollow data file.", parts[0]);
    DWORD count = rd32(a + 8);
    progress_total = count + 12; /* entries + manual pages + readme + runtime + registry */
    mkdirs(dest_dir);
    char logp[MAX_PATH];
    snprintf(logp, sizeof logp, "%s\\unins000.dat", dest_dir);
    log_out = fopen(logp, "w");
    if (!log_out) return fail(4, "Setup cannot write to %s.", dest_dir);

    for (DWORD i = 0; i < count; i++) {
        if (off + 2 > total) goto truncated;
        unsigned nl = a[off] | a[off + 1] << 8;
        if (off + 2 + nl + 8 > total) goto truncated;
        char name[MAX_PATH] = {0};
        memcpy(name, a + off + 2, nl < MAX_PATH - 1 ? nl : MAX_PATH - 1);
        DWORD size = rd32(a + off + 2 + nl), hash = rd32(a + off + 6 + nl);
        size_t data = off + 10 + nl;
        if (data + size > total) goto truncated;
        if (fnv1a(a + data, size) != hash)
            return fail(3, "The file %s is corrupt (checksum mismatch in %s).\r\n\r\n"
                           "Download it again and run Setup again.",
                        parts[data + size > len[0]], name);
        if (!write_file(name, a + data, size)) return fail(4, "Setup cannot write %s.", name);
        off = data + size;
        step(name);
        Sleep(60); /* lets a person watch the progress bar */
    }
    if (!copy_tree("manual")) return fail(4, "Setup cannot copy the manual.");
    size_t rn;
    unsigned char *readme = read_all("readme.txt", &rn);
    if (!readme || !write_file("readme.txt", readme, rn)) return fail(4, "Setup cannot copy readme.txt.");
    step("readme.txt");
    fprintf(log_out, "unins000.dat\n");
    fclose(log_out);

    step("Installing Lantern Hollow Runtime...");
    int rc = run_prereq("/passive", 1);
    if (rc != 0) return fail(4, "The Lantern Hollow Runtime setup failed (code %d).", rc);
    /* The runtime finishes its setup in the background, after this wizard has exited. */
    if (run_prereq("/finalize", 0) < 0) return fail(4, "Cannot start the runtime finalizer.");

    HKEY k;
    if (RegCreateKeyExA(HKEY_LOCAL_MACHINE, LH_UNINSTALL_KEY, 0, NULL, 0, KEY_WRITE, NULL, &k,
                        NULL))
        return fail(4, "Setup cannot write the registry.");
    char un[MAX_PATH + 2];
    snprintf(un, sizeof un, "\"%s\\unins000.exe\"", dest_dir);
    RegSetValueExA(k, "DisplayName", 0, REG_SZ, (BYTE *)"Lantern Hollow", 15);
    RegSetValueExA(k, "DisplayVersion", 0, REG_SZ, (BYTE *)"1.0", 4);
    RegSetValueExA(k, "UninstallString", 0, REG_SZ, (BYTE *)un, strlen(un) + 1);
    RegSetValueExA(k, "InstallLocation", 0, REG_SZ, (BYTE *)dest_dir, strlen(dest_dir) + 1);
    RegCloseKey(k);
    step("Done");
    return 0;

truncated:
    return fail(3, "The file %s is incomplete. Download it again and run Setup again.", parts[1]);
}

static DWORD WINAPI worker(LPVOID unused) {
    (void)unused;
    PostMessageA(wnd, WM_LH_DONE, install(), 0);
    return 0;
}

/* ---- audio: a short chime while installing, and what this process could see ---- */

static void chime_and_report(void) {
    DWORD n;
    unsigned char *wav = make_wav(&n);
    BOOL played = PlaySoundA((LPCSTR)wav, NULL, SND_MEMORY | SND_SYNC | SND_NODEFAULT);
    free(wav);
    char p[MAX_PATH];
    appdata_dir(p);
    lstrcatA(p, "\\setup-audio.txt");
    FILE *f = fopen(p, "w");
    if (!f) return;
    audio_report(f);
    fprintf(f, "playsound=%d\n", played);
    fclose(f);
}

static DWORD WINAPI chime_thread(LPVOID unused) {
    (void)unused;
    chime_and_report();
    return 0;
}

/* ---- autoclick ---- */

static void autostep(void) {
    const char *ac = autoclick();
    switch (page) {
    case P_WELCOME: PostMessageA(next, BM_CLICK, 0, 0); break;
    case P_LICENSE:
        if (!strcmp(ac, "cancel")) {
            PostMessageA(cancel, BM_CLICK, 0, 0);
        } else if (SendMessageA(accept_box, BM_GETCHECK, 0, 0) != BST_CHECKED) {
            /* Proves the gate: Next is disabled until the box is ticked. */
            trace("license next_enabled_before_accept=%d", IsWindowEnabled(next));
            PostMessageA(next, BM_CLICK, 0, 0); /* ignored while disabled */
            PostMessageA(accept_box, BM_CLICK, 0, 0);
        } else {
            PostMessageA(next, BM_CLICK, 0, 0);
        }
        break;
    case P_DEST:
    case P_FINISH: PostMessageA(next, BM_CLICK, 0, 0); break;
    }
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    switch (m) {
    case WM_PAINT: {
        PAINTSTRUCT ps;
        HDC dc = BeginPaint(h, &ps);
        RECT r = {0, 0, 164, 314};
        paint_lantern(dc, r);
        EndPaint(h, &ps);
        return 0;
    }
    case WM_CTLCOLORSTATIC:
        SetBkMode((HDC)w, TRANSPARENT);
        return (LRESULT)GetSysColorBrush(COLOR_WINDOW);
    case WM_TIMER: autostep(); return 0;
    case WM_COMMAND:
        switch (LOWORD(w)) {
        case IDC_ACCEPT:
            button(SendMessageA(accept_box, BM_GETCHECK, 0, 0) == BST_CHECKED ? "accept-on"
                                                                                  : "accept-off");
            EnableWindow(next, SendMessageA(accept_box, BM_GETCHECK, 0, 0) == BST_CHECKED);
            return 0;
        case IDC_BACK:
            button("back");
            set_page(page - 1);
            return 0;
        case IDC_CANCEL:
            button("cancel");
            PostQuitMessage(2);
            return 0;
        case IDC_NEXT:
            if (page == P_LICENSE && SendMessageA(accept_box, BM_GETCHECK, 0, 0) != BST_CHECKED) {
                trace("press license next-ignored");
                return 0;
            }
            button(page == P_DEST ? "install" : page == P_FINISH ? "finish" : "next");
            if (page == P_DEST) {
                GetWindowTextA(dest, dest_dir, sizeof dest_dir);
                size_t n = strlen(dest_dir);
                while (n > 3 && dest_dir[n - 1] == '\\') dest_dir[--n] = 0;
                if (n < 3 || dest_dir[1] != ':') {
                    MessageBoxA(h, "Please enter a full path, for example C:\\Games\\Lantern Hollow.",
                                TITLE, MB_OK | MB_ICONWARNING);
                    return 0;
                }
                set_page(P_INSTALL);
                CloseHandle(CreateThread(NULL, 0, chime_thread, NULL, 0, NULL));
                CloseHandle(CreateThread(NULL, 0, worker, NULL, 0, NULL));
            } else if (page == P_FINISH) {
                if (SendMessageA(launch, BM_GETCHECK, 0, 0) == BST_CHECKED) {
                    char exe[MAX_PATH + 32];
                    snprintf(exe, sizeof exe, "%s\\LanternHollow.exe", dest_dir);
                    STARTUPINFOA si = {sizeof si};
                    PROCESS_INFORMATION pi;
                    if (CreateProcessA(exe, NULL, NULL, NULL, FALSE, 0, NULL, dest_dir, &si, &pi)) {
                        CloseHandle(pi.hProcess);
                        CloseHandle(pi.hThread);
                    }
                }
                PostQuitMessage(0);
            } else {
                set_page(page + 1);
            }
            return 0;
        }
        break;
    case WM_LH_PROGRESS:
        SendMessageA(progress, PBM_SETPOS, w, 0);
        SetWindowTextA(status, (char *)l);
        free((char *)l);
        return 0;
    case WM_LH_DONE:
        if (w == 0) {
            set_page(P_FINISH);
        } else {
            trace("error %s", fail_msg);
            answer_later(TITLE, IDOK);
            MessageBoxA(h, fail_msg, TITLE, MB_OK | MB_ICONERROR);
            PostQuitMessage((int)w);
        }
        return 0;
    case WM_CLOSE:
        if (page == P_INSTALL) return 0;
        trace("press %s close", page_names[page]);
        PostQuitMessage(page == P_FINISH ? 0 : 2);
        return 0;
    }
    return DefWindowProcA(h, m, w, l);
}

static HWND ctl(const char *cls, const char *text, DWORD style, int x, int y, int cx, int cy, int id) {
    HWND c = CreateWindowExA(!strcmp(cls, "EDIT") ? WS_EX_CLIENTEDGE : 0, cls, text,
                             WS_CHILD | WS_VISIBLE | style, x, y, cx, cy, wnd, (HMENU)(INT_PTR)id,
                             NULL, NULL);
    SendMessageA(c, WM_SETFONT, (WPARAM)GetStockObject(DEFAULT_GUI_FONT), TRUE);
    return c;
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmdline, int show_cmd) {
    (void)prev;
    (void)cmdline;
    INITCOMMONCONTROLSEX icc = {sizeof icc, ICC_PROGRESS_CLASS};
    InitCommonControlsEx(&icc);
    char cwd[MAX_PATH];
    GetCurrentDirectoryA(sizeof cwd, cwd);
    trace("start cwd=%s autoclick=%s", cwd, autoclick() ? autoclick() : "(unset)");

    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.hInstance = inst;
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.hbrBackground = GetSysColorBrush(COLOR_WINDOW);
    wc.lpszClassName = "LanternHollowSetup";
    RegisterClassA(&wc);
    RECT r = {0, 0, 500, 360};
    AdjustWindowRect(&r, WS_CAPTION | WS_SYSMENU, FALSE);
    wnd = CreateWindowA("LanternHollowSetup", TITLE, WS_CAPTION | WS_SYSMENU, CW_USEDEFAULT,
                        CW_USEDEFAULT, r.right - r.left, r.bottom - r.top, NULL, NULL, inst, NULL);

    bold = CreateFontA(-15, 0, 0, 0, FW_BOLD, 0, 0, 0, DEFAULT_CHARSET, 0, 0, 0, 0, "Tahoma");
    title = ctl("STATIC", "", 0, 180, 16, 304, 40, 0);
    SendMessageA(title, WM_SETFONT, (WPARAM)bold, TRUE);
    body = ctl("STATIC", "", 0, 180, 62, 304, 70, 0);
    size_t ln;
    unsigned char *lic = read_all("license.txt", &ln);
    license = ctl("EDIT", lic ? (char *)lic : "license.txt is missing.",
                  ES_MULTILINE | ES_READONLY | WS_VSCROLL, 180, 120, 304, 140, 0);
    free(lic);
    accept_box = ctl("BUTTON", "I &accept the agreement", BS_AUTOCHECKBOX, 180, 268, 304, 20, IDC_ACCEPT);
    dest = ctl("EDIT", DEFAULT_DEST, ES_AUTOHSCROLL, 180, 140, 304, 22, IDC_DEST);
    status = ctl("STATIC", "", 0, 180, 140, 304, 20, 0);
    progress = ctl(PROGRESS_CLASSA, "", 0, 180, 164, 304, 18, 0);
    SendMessageA(progress, PBM_SETRANGE32, 0, 1000);
    launch = ctl("BUTTON", "&Launch Lantern Hollow", BS_AUTOCHECKBOX, 180, 150, 304, 20, IDC_LAUNCH);
    ctl("STATIC", "", SS_ETCHEDHORZ, 0, 314, 500, 2, 0);
    back = ctl("BUTTON", "< &Back", BS_PUSHBUTTON, 236, 326, 80, 24, IDC_BACK);
    next = ctl("BUTTON", "&Next >", BS_DEFPUSHBUTTON, 318, 326, 80, 24, IDC_NEXT);
    cancel = ctl("BUTTON", "Cancel", BS_PUSHBUTTON, 410, 326, 80, 24, IDC_CANCEL);

    set_page(P_WELCOME);
    ShowWindow(wnd, show_cmd);
    UpdateWindow(wnd);
    if (autoclick()) SetTimer(wnd, 1, 700, NULL);

    MSG msg;
    while (GetMessageA(&msg, NULL, 0, 0) > 0) {
        if (!IsDialogMessageA(wnd, &msg)) {
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
    }
    trace("exit %d", (int)msg.wParam);
    return (int)msg.wParam;
}
