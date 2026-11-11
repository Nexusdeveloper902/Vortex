/* Host tool (built with the host cc by build.sh): writes the Lantern Hollow data archive.
 *
 *   lhpack <out-prefix> <LanternHollow.exe> <unins000.exe>
 *
 * Archive stream (little endian): "LHPK", u32 version = 1, u32 count, then per entry
 * u16 name length, name (Windows relative path), u32 size, u32 FNV-1a of the data, data.
 * The stream is split in the middle into <out-prefix>-1.bin and <out-prefix>-2.bin, so one
 * entry's data spans both parts. Game data is generated deterministically. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned fnv1a(const unsigned char *p, size_t n) {
    unsigned h = 2166136261u;
    while (n--) {
        h ^= *p++;
        h *= 16777619u;
    }
    return h;
}

struct entry {
    const char *name;
    unsigned char *data;
    size_t size;
};

static unsigned char *slurp(const char *path, size_t *n) {
    FILE *f = fopen(path, "rb");
    if (!f) {
        perror(path);
        exit(1);
    }
    fseek(f, 0, SEEK_END);
    *n = ftell(f);
    fseek(f, 0, SEEK_SET);
    unsigned char *b = malloc(*n);
    if (fread(b, 1, *n, f) != *n) exit(1);
    fclose(f);
    return b;
}

static unsigned char *noise(size_t n, unsigned seed) {
    unsigned char *b = malloc(n);
    for (size_t i = 0; i < n; i++) {
        seed = seed * 1103515245u + 12345u;
        b[i] = seed >> 16;
    }
    return b;
}

static unsigned char *buf;
static size_t len, cap;

static void put(const void *p, size_t n) {
    if (len + n > cap) {
        cap = (len + n) * 2;
        buf = realloc(buf, cap);
    }
    memcpy(buf + len, p, n);
    len += n;
}

static void put32(unsigned v) {
    unsigned char b[4] = {v, v >> 8, v >> 16, v >> 24};
    put(b, 4);
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: lhpack <out-prefix> <LanternHollow.exe> <unins000.exe>\n");
        return 2;
    }
    static const char strings[] =
        "title=Lantern Hollow\r\n"
        "intro=The valley goes dark at dusk. Light the lanterns before the fog rolls in.\r\n"
        "hint=Oil is scarce. The old mill keeps a spare can.\r\n";
    struct entry e[6];
    e[0] = (struct entry){"LanternHollow.exe", NULL, 0};
    e[0].data = slurp(argv[2], &e[0].size);
    e[1] = (struct entry){"data\\world.dat", noise(300000, 7), 300000};
    e[2] = (struct entry){"data\\lanterns.dat", noise(70000, 11), 70000};
    e[3] = (struct entry){"data\\strings.txt", (unsigned char *)strings, sizeof strings - 1};
    char man[512];
    int mn = 0;
    for (int i = 1; i <= 3; i++)
        mn += snprintf(man + mn, sizeof man - mn, "%08x %s\r\n", fnv1a(e[i].data, e[i].size),
                       e[i].name);
    e[4] = (struct entry){"data\\manifest.txt", (unsigned char *)man, (size_t)mn};
    e[5] = (struct entry){"unins000.exe", NULL, 0};
    e[5].data = slurp(argv[3], &e[5].size);

    size_t start[6];
    put("LHPK", 4);
    put32(1);
    put32(6);
    for (int i = 0; i < 6; i++) {
        unsigned short nl = strlen(e[i].name);
        unsigned char l2[2] = {nl, nl >> 8};
        put(l2, 2);
        put(e[i].name, nl);
        put32(e[i].size);
        put32(fnv1a(e[i].data, e[i].size));
        start[i] = len;
        put(e[i].data, e[i].size);
    }
    size_t half = len / 2;
    char path[4096];
    for (int part = 1; part <= 2; part++) {
        snprintf(path, sizeof path, "%s-%d.bin", argv[1], part);
        FILE *f = fopen(path, "wb");
        if (!f) {
            perror(path);
            return 1;
        }
        size_t from = part == 1 ? 0 : half, n = part == 1 ? half : len - half;
        if (fwrite(buf + from, 1, n, f) != n) return 1;
        fclose(f);
    }
    for (int i = 0; i < 6; i++)
        if (start[i] < half && start[i] + e[i].size > half)
            printf("split at %zu of %zu bytes, inside %s\n", half, len, e[i].name);
    return 0;
}
