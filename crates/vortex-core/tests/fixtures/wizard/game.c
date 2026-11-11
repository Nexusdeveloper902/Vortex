/* LanternHollow.exe: the fictional game. Loads its data by RELATIVE path from the current
 * directory (data\manifest.txt lists "<fnv1a hex> <file>"), verifies every file, keeps a save in
 * %APPDATA%\LanternHollow\save.dat and records the run in last_run.txt (plus game-audio.txt).
 * Exits 0 when the data verified, 4 otherwise. Closes itself after 3 s when LH_AUTOCLICK is set,
 * otherwise when the window is closed. */
#include "lh.h"

static char line1[128], line2[160], line3[160];
static int status = 4;

static void load(void) {
    char cwd[MAX_PATH], dir[MAX_PATH], p[MAX_PATH];
    GetCurrentDirectoryA(sizeof cwd, cwd);
    int total = 0, ok = 0;
    size_t n;
    char *man = (char *)read_all("data\\manifest.txt", &n);
    for (char *l = man ? strtok(man, "\r\n") : NULL; l; l = strtok(NULL, "\r\n")) {
        unsigned want;
        char name[MAX_PATH];
        if (sscanf(l, "%x %259[^\r\n]", &want, name) != 2) continue;
        total++;
        size_t fn;
        unsigned char *b = read_all(name, &fn);
        if (b && fnv1a(b, fn) == want) ok++;
        free(b);
    }
    free(man);
    status = total > 0 && ok == total ? 0 : 4;

    appdata_dir(dir);
    int runs = 0;
    snprintf(p, sizeof p, "%s\\save.dat", dir);
    char *save = (char *)read_all(p, NULL);
    if (save) sscanf(save, "LHSAVE1 lanterns_lit=%d", &runs);
    free(save);
    runs++;
    FILE *f = fopen(p, "w");
    if (f) {
        fprintf(f, "LHSAVE1 lanterns_lit=%d\n", runs);
        fclose(f);
    }
    snprintf(p, sizeof p, "%s\\last_run.txt", dir);
    f = fopen(p, "w");
    if (f) {
        fprintf(f, "cwd=%s\nverified=%d/%d\nstatus=%s\nlanterns_lit=%d\n", cwd, ok, total,
                status ? "FAIL" : "OK", runs);
        fclose(f);
    }
    snprintf(p, sizeof p, "%s\\game-audio.txt", dir);
    f = fopen(p, "w");
    if (f) {
        audio_report(f);
        fclose(f);
    }
    snprintf(line1, sizeof line1, "Lantern Hollow");
    snprintf(line2, sizeof line2, "Data files verified: %d of %d", ok, total);
    snprintf(line3, sizeof line3, "Lanterns lit so far: %d (saved)", runs);
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    switch (m) {
    case WM_PAINT: {
        PAINTSTRUCT ps;
        HDC dc = BeginPaint(h, &ps);
        RECT r;
        GetClientRect(h, &r);
        paint_lantern(dc, r);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, RGB(240, 226, 190));
        RECT t = {0, r.bottom - 110, r.right, r.bottom};
        char all[512];
        snprintf(all, sizeof all, "%s\n%s\n%s", line1, line2, line3);
        DrawTextA(dc, all, -1, &t, DT_CENTER);
        EndPaint(h, &ps);
        return 0;
    }
    case WM_TIMER: DestroyWindow(h); return 0;
    case WM_DESTROY: PostQuitMessage(status); return 0;
    }
    return DefWindowProcA(h, m, w, l);
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmdline, int show_cmd) {
    (void)prev;
    (void)cmdline;
    load();
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.hInstance = inst;
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.lpszClassName = "LanternHollow";
    RegisterClassA(&wc);
    HWND h = CreateWindowA("LanternHollow", "Lantern Hollow", WS_OVERLAPPEDWINDOW, CW_USEDEFAULT,
                           CW_USEDEFAULT, 520, 400, NULL, NULL, inst, NULL);
    ShowWindow(h, show_cmd);
    if (autoclick()) SetTimer(h, 1, 3000, NULL);
    MSG msg;
    while (GetMessageA(&msg, NULL, 0, 0) > 0) {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
    return (int)msg.wParam;
}
