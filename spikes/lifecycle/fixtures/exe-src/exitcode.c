/* M1 spike: prints to stdout and stderr, exits with argv[1] (default 7). */
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv) { printf("stdout-line\n"); fprintf(stderr, "stderr-line\n"); return argc > 1 ? atoi(argv[1]) : 7; }
