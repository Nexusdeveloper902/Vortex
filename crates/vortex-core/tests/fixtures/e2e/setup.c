/* M14 multi-file installer. Records its cwd, needs data1.bin, data2.bin and setup-1.bin (plus
 * data3.bin when built with -DNEED_DATA3) by relative path, copies the data and the game into
 * C:\Program Files\E2E Multi Game (with an uninstaller next to it), starts Redist\prereq.exe
 * detached and exits 0 without waiting for it. */
#include <windows.h>
#include <stdio.h>
#include <string.h>
#define DEST "C:\\Program Files\\E2E Multi Game"
static int have(const char *f) {
    FILE *d = fopen(f, "rb");
    if (!d) return 0;
    fclose(d);
    return 1;
}
int main(void) {
    char mod[MAX_PATH], cwd[MAX_PATH], src[MAX_PATH], cmd[2 * MAX_PATH];
    GetModuleFileNameA(NULL, mod, MAX_PATH);
    *strrchr(mod, '\\') = 0;
    GetCurrentDirectoryA(MAX_PATH, cwd);
    FILE *r = fopen("C:\\vortex-setup.txt", "w");
    if (!r) return 2;
    fprintf(r, "cwd=%s\nmodule_dir=%s\n", cwd, mod);
    fclose(r);
    if (!have("data1.bin") || !have("data2.bin") || !have("setup-1.bin")) return 3;
#ifdef NEED_DATA3
    if (!have("data3.bin")) return 3;
#endif
    CreateDirectoryA(DEST, NULL);
    CreateDirectoryA(DEST "\\data", NULL);
    if (!CopyFileA("data1.bin", DEST "\\data\\data1.bin", FALSE)) return 4;
    if (!CopyFileA("data2.bin", DEST "\\data\\data2.bin", FALSE)) return 4;
    snprintf(src, MAX_PATH, "%s\\payload\\Game.dat", mod);
    if (!CopyFileA(src, DEST "\\Game.exe", FALSE)) return 5;
    if (!CopyFileA(src, DEST "\\unins000.exe", FALSE)) return 5;
    snprintf(cmd, sizeof cmd, "\"%s\\Redist\\prereq.exe\"", mod);
    STARTUPINFOA si = { sizeof si };
    PROCESS_INFORMATION pi;
    if (!CreateProcessA(NULL, cmd, NULL, NULL, FALSE, DETACHED_PROCESS, NULL, NULL, &si, &pi))
        return 6;
    return 0;
}
