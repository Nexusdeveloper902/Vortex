/* M14 prerequisite ("vcredist-like"): outlives the installer by SLEEP_MS, then writes
 * C:\vortex-prereq.done. */
#include <windows.h>
#include <stdio.h>
#ifndef SLEEP_MS
#define SLEEP_MS 5000
#endif
int main(void) {
    Sleep(SLEEP_MS);
    FILE *f = fopen("C:\\vortex-prereq.done", "w");
    if (!f) return 2;
    fputs("done\n", f);
    fclose(f);
    return 0;
}
