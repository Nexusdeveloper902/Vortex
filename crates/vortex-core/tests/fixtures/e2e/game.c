/* M14 game fixture.
 * Named *setup*.exe it is a standalone installer: it copies itself to
 * C:\Program Files\E2E Standalone\Standalone.exe and exits 0.
 * Otherwise it is the game: it writes C:\vortex-report-<exe name>.txt with its module path, cwd,
 * every argument, $VORTEX_E2E_VAR and the first line of data\data1.bin (relative to its cwd).
 * $VORTEX_E2E_MODE: "crash" dereferences NULL, "exit:<n>" exits with n. */
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
int main(int argc, char **argv) {
    char mod[MAX_PATH], low[MAX_PATH], cwd[MAX_PATH], out[2 * MAX_PATH], line[256] = "(missing)";
    GetModuleFileNameA(NULL, mod, MAX_PATH);
    const char *name = strrchr(mod, '\\') + 1;
    size_t i;
    for (i = 0; name[i] && i < MAX_PATH - 1; i++) low[i] = (char)tolower((unsigned char)name[i]);
    low[i] = 0;
    if (strstr(low, "setup")) {
        CreateDirectoryA("C:\\Program Files\\E2E Standalone", NULL);
        return CopyFileA(mod, "C:\\Program Files\\E2E Standalone\\Standalone.exe", FALSE) ? 0 : 4;
    }
    GetCurrentDirectoryA(MAX_PATH, cwd);
    FILE *d = fopen("data\\data1.bin", "rb");
    if (d) {
        if (!fgets(line, sizeof line, d)) strcpy(line, "(empty)");
        line[strcspn(line, "\r\n")] = 0;
        fclose(d);
    }
    snprintf(out, sizeof out, "C:\\vortex-report-%s.txt", name);
    FILE *f = fopen(out, "w");
    if (!f) return 2;
    fprintf(f, "module=%s\ncwd=%s\n", mod, cwd);
    for (int a = 1; a < argc; a++) fprintf(f, "arg=%s\n", argv[a]);
    const char *v = getenv("VORTEX_E2E_VAR");
    fprintf(f, "env=%s\ndata1=%s\n", v ? v : "(unset)", line);
    fclose(f);
    const char *m = getenv("VORTEX_E2E_MODE");
    if (m && !strcmp(m, "crash")) *(volatile int *)0 = 1;
    if (m && !strncmp(m, "exit:", 5)) return atoi(m + 5);
    return 0;
}
