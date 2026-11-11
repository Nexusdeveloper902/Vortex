/* redist\prereq_setup.exe: the fictional "Lantern Hollow Runtime" prerequisite.
 *   /passive   shows a small progress window for ~2 s, writes installed.txt, exits 0
 *              (setup_lantern_hollow.exe waits for this one)
 *   /finalize  no UI; sleeps 6 s, then writes finalize.txt (started detached; it outlives the
 *              wizard, so Vortex must keep waiting for it)
 * Files go to C:\ProgramData\LanternHollow Runtime. */
#include "lh.h"

#define DIR "C:\\ProgramData\\LanternHollow Runtime"

static void mark(const char *name) {
    mkdirs(DIR);
    char p[MAX_PATH];
    snprintf(p, sizeof p, DIR "\\%s", name);
    FILE *f = fopen(p, "w");
    if (f) {
        fprintf(f, "Lantern Hollow Runtime 1.0: %s\n", name);
        fclose(f);
    }
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_TIMER) {
        DestroyWindow(h);
        return 0;
    }
    if (m == WM_DESTROY) {
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(h, m, w, l);
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmdline, int show_cmd) {
    (void)prev;
    trace("prereq start %s", cmdline);
    if (strstr(cmdline, "/finalize")) {
        Sleep(6000);
        mark("finalize.txt");
        trace("prereq finalize done");
        return 0;
    }
    INITCOMMONCONTROLSEX icc = {sizeof icc, ICC_PROGRESS_CLASS};
    InitCommonControlsEx(&icc);
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.hInstance = inst;
    wc.hbrBackground = GetSysColorBrush(COLOR_BTNFACE);
    wc.lpszClassName = "LHRuntimeSetup";
    RegisterClassA(&wc);
    HWND h = CreateWindowA("LHRuntimeSetup", "Lantern Hollow Runtime Setup",
                           WS_CAPTION | WS_VISIBLE, CW_USEDEFAULT, CW_USEDEFAULT, 360, 120, NULL,
                           NULL, inst, NULL);
    HWND t = CreateWindowA("STATIC", "Installing runtime components...", WS_CHILD | WS_VISIBLE, 16,
                           12, 320, 20, h, NULL, inst, NULL);
    SendMessageA(t, WM_SETFONT, (WPARAM)GetStockObject(DEFAULT_GUI_FONT), TRUE);
    HWND p = CreateWindowA(PROGRESS_CLASSA, "", WS_CHILD | WS_VISIBLE | PBS_MARQUEE, 16, 40, 320,
                           18, h, NULL, inst, NULL);
    SendMessageA(p, PBM_SETMARQUEE, TRUE, 30);
    ShowWindow(h, show_cmd);
    SetTimer(h, 1, 2000, NULL);
    MSG msg;
    while (GetMessageA(&msg, NULL, 0, 0) > 0) {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
    mark("installed.txt");
    trace("prereq passive done");
    return 0;
}
