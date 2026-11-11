/* M19 verbose-log fixture: opens data3.bin by relative path and exits 3 when it is missing. */
#include <stdio.h>
int main(void) {
    FILE *f = fopen("data3.bin", "rb");
    if (!f) return 3;
    fclose(f);
    return 0;
}
