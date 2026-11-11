/* unins000.exe: Lantern Hollow's uninstaller, in two phases like common real uninstallers.
 *
 * Phase 1 (started from the install dir): copies itself to %TEMP%\_lhXXXX.tmp, starts that copy
 * with /SECONDPHASE="<install dir>" /PARENT=<pid>, and exits 0 at once. A launcher that only
 * waits for the process it started would report "finished" here.
 * Phase 2 (the %TEMP% copy): waits for phase 1 to exit, asks "Remove Lantern Hollow?" (Yes/No),
 * deletes the files listed in unins000.dat (then the empty folders, unins000.dat, unins000.exe
 * and the install dir), removes the Uninstall registry key, keeps %APPDATA%\LanternHollow (the
 * save), tells the user, and schedules its own deletion. Exit codes: 0 removed, 2 user said No.
 * LH_AUTOCLICK answers the dialogs. */
#include "lh.h"

#define TITLE "Lantern Hollow Uninstall"

static int phase1(void) {
    char self[MAX_PATH], tmpdir[MAX_PATH], tmp[MAX_PATH], dir[MAX_PATH];
    GetModuleFileNameA(NULL, self, sizeof self);
    lstrcpyA(dir, self);
    *strrchr(dir, '\\') = 0;
    GetTempPathA(sizeof tmpdir, tmpdir);
    if (!GetTempFileNameA(tmpdir, "_lh", 0, tmp) || !CopyFileA(self, tmp, FALSE)) {
        MessageBoxA(NULL, "Cannot copy the uninstaller to the temporary folder.", TITLE,
                    MB_OK | MB_ICONERROR);
        return 1;
    }
    char cmd[3 * MAX_PATH];
    snprintf(cmd, sizeof cmd, "\"%s\" /SECONDPHASE=\"%s\" /PARENT=%lu", tmp, dir,
             GetCurrentProcessId());
    STARTUPINFOA si = {sizeof si};
    PROCESS_INFORMATION pi;
    if (!CreateProcessA(tmp, cmd, NULL, NULL, FALSE, 0, NULL, tmpdir, &si, &pi)) return 1;
    trace("unins phase1 started %s", tmp);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    return 0;
}

static HWND status, bar;

static void pump(void) {
    MSG msg;
    while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
}

/* Removes `rel`'s parent folders inside `dir` while they are empty. */
static void prune(const char *dir, const char *rel) {
    char p[MAX_PATH];
    snprintf(p, sizeof p, "%s\\%s", dir, rel);
    for (char *s = strrchr(p, '\\'); s && (size_t)(s - p) > strlen(dir); s = strrchr(p, '\\')) {
        *s = 0;
        if (!RemoveDirectoryA(p)) break;
    }
}

static int phase2(const char *dir, DWORD parent) {
    HANDLE ph = OpenProcess(SYNCHRONIZE, FALSE, parent);
    if (ph) {
        WaitForSingleObject(ph, 30000);
        CloseHandle(ph);
    }
    trace("unins phase2 dir=%s", dir);
    answer_later(TITLE, IDYES);
    if (MessageBoxA(NULL,
                    "Are you sure you want to completely remove Lantern Hollow and all of its "
                    "components?\r\n\r\nYour saved games are kept.",
                    TITLE, MB_YESNO | MB_ICONQUESTION) != IDYES) {
        trace("unins phase2 answer=no");
        return 2;
    }
    trace("unins phase2 answer=yes");

    INITCOMMONCONTROLSEX icc = {sizeof icc, ICC_PROGRESS_CLASS};
    InitCommonControlsEx(&icc);
    HWND w = CreateWindowA("STATIC", TITLE, WS_POPUP | WS_CAPTION | WS_VISIBLE, CW_USEDEFAULT,
                           CW_USEDEFAULT, 380, 110, NULL, NULL, NULL, NULL);
    status = CreateWindowA("STATIC", "Uninstalling Lantern Hollow...", WS_CHILD | WS_VISIBLE, 16,
                           12, 340, 20, w, NULL, NULL, NULL);
    SendMessageA(status, WM_SETFONT, (WPARAM)GetStockObject(DEFAULT_GUI_FONT), TRUE);
    bar = CreateWindowA(PROGRESS_CLASSA, "", WS_CHILD | WS_VISIBLE, 16, 40, 340, 18, w, NULL,
                        NULL, NULL);

    char p[MAX_PATH];
    snprintf(p, sizeof p, "%s\\unins000.dat", dir);
    size_t n;
    char *list = (char *)read_all(p, &n);
    int total = 0, done = 0;
    for (size_t i = 0; list && i < n; i++) total += list[i] == '\n';
    SendMessageA(bar, PBM_SETRANGE32, 0, total ? total : 1);
    for (char *l = list ? strtok(list, "\r\n") : NULL; l; l = strtok(NULL, "\r\n")) {
        if (strstr(l, "..")) continue; /* never leave the install dir */
        snprintf(p, sizeof p, "%s\\%s", dir, l);
        if (!DeleteFileA(p)) trace("unins delete failed %s err=%lu", l, GetLastError());
        prune(dir, l);
        SendMessageA(bar, PBM_SETPOS, ++done, 0);
        pump();
        Sleep(150); /* a real uninstaller is not instant; Vortex must keep waiting */
    }
    free(list);
    snprintf(p, sizeof p, "%s\\unins000.exe", dir);
    DeleteFileA(p);
    if (!RemoveDirectoryA(dir)) trace("unins rmdir failed err=%lu", GetLastError());
    RegDeleteKeyA(HKEY_LOCAL_MACHINE, LH_UNINSTALL_KEY);
    DestroyWindow(w);
    trace("unins phase2 removed dir_left=%d", GetFileAttributesA(dir) != INVALID_FILE_ATTRIBUTES);

    answer_later(TITLE, IDOK);
    MessageBoxA(NULL, "Lantern Hollow was successfully removed from your computer.", TITLE,
                MB_OK | MB_ICONINFORMATION);
    char self[MAX_PATH];
    GetModuleFileNameA(NULL, self, sizeof self);
    MoveFileExA(self, NULL, MOVEFILE_DELAY_UNTIL_REBOOT);
    trace("unins phase2 exit");
    return 0;
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmdline, int show_cmd) {
    (void)inst;
    (void)prev;
    (void)show_cmd;
    trace("unins start %s", cmdline);
    char *s = strstr(cmdline, "/SECONDPHASE=\"");
    if (!s) return phase1();
    char dir[MAX_PATH];
    s += strlen("/SECONDPHASE=\"");
    char *e = strchr(s, '"');
    if (!e || e - s >= MAX_PATH) return 1;
    memcpy(dir, s, e - s);
    dir[e - s] = 0;
    char *pp = strstr(cmdline, "/PARENT=");
    return phase2(dir, pp ? strtoul(pp + 8, NULL, 10) : 0);
}
