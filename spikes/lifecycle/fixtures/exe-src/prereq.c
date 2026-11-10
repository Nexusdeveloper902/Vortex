/* M1 spike fixture: sleeps 15 s then writes prereq.done (with the marker it sees) next to itself. */
#include <windows.h>
#include <stdio.h>
#include <string.h>
int main(void) {
    char mod[MAX_PATH], mk[64] = "NONE";
    Sleep(15000);
    GetModuleFileNameA(NULL, mod, MAX_PATH);
    strcpy(strrchr(mod, '\\'), "\\prereq.done");
    GetEnvironmentVariableA("VORTEX_RUN_ID", mk, sizeof mk);
    FILE *f = fopen(mod, "w");
    fprintf(f, "marker=%s\n", mk);
    fclose(f);
    return 0;
}
