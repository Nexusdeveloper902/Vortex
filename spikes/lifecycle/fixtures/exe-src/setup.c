/* M1 spike fixture. setup.exe [detached|noenv|breakaway]
   Records cwd, module dir, relative data1.bin access (cwd and module dir), marker env,
   then spawns Redist\prereq.exe with the given creation mode and exits at once. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    char cwd[MAX_PATH], mod[MAX_PATH], path[MAX_PATH], mk[64] = "NONE";
    const char *mode = argc > 1 ? argv[1] : "detached";
    GetCurrentDirectoryA(MAX_PATH, cwd);
    GetModuleFileNameA(NULL, mod, MAX_PATH);
    *strrchr(mod, '\\') = 0;
    GetEnvironmentVariableA("VORTEX_RUN_ID", mk, sizeof mk);
    snprintf(path, MAX_PATH, "%s\\result.txt", mod);
    FILE *r = fopen(path, "w");
    fprintf(r, "mode=%s\ncwd=%s\nmoddir=%s\nmarker=%s\n", mode, cwd, mod, mk);
    FILE *d = fopen("data1.bin", "rb");
    char buf[64] = {0};
    if (d) { fgets(buf, sizeof buf, d); fclose(d); }
    fprintf(r, "data1_rel_cwd=%s %s", d ? "found" : "missing", d ? buf : "\n");
    snprintf(path, MAX_PATH, "%s\\data1.bin", mod);
    d = fopen(path, "rb");
    fprintf(r, "data1_moddir=%s\n", d ? "found" : "missing");
    if (d) fclose(d);

    char cmd[MAX_PATH];
    snprintf(cmd, MAX_PATH, "\"%s\\Redist\\prereq.exe\"", mod);
    DWORD flags = 0;
    LPVOID env = NULL;
    static char block[] = "PATH=C:\\windows\\system32\0SYSTEMROOT=C:\\windows\0VTEST=custom\0";
    if (!strcmp(mode, "detached")) flags = DETACHED_PROCESS;
    else if (!strcmp(mode, "noenv")) { flags = DETACHED_PROCESS; env = block; }
    else if (!strcmp(mode, "breakaway")) flags = DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP;
    STARTUPINFOA si = { sizeof si };
    PROCESS_INFORMATION pi;
    BOOL ok = CreateProcessA(NULL, cmd, NULL, NULL, FALSE, flags, env, NULL, &si, &pi);
    fprintf(r, "spawn=%s err=%lu pid=%lu flags=0x%lx\n", ok ? "ok" : "FAILED", ok ? 0 : GetLastError(),
            ok ? pi.dwProcessId : 0, flags);
    fclose(r);
    return 0; /* exit immediately; prereq keeps running */
}
