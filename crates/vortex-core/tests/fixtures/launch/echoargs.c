/* Writes its argv (one per line) and $VORTEX_TEST_VAR to args.txt in its cwd. */
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv) {
    FILE *f = fopen("args.txt", "w");
    if (!f) return 2;
    for (int i = 1; i < argc; i++) fprintf(f, "arg=%s\n", argv[i]);
    const char *v = getenv("VORTEX_TEST_VAR");
    fprintf(f, "env=%s\n", v ? v : "(unset)");
    fclose(f);
    return 0;
}
