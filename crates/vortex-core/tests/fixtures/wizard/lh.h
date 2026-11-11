/* Shared helpers for the fictional "Lantern Hollow" installer, game and uninstaller (M16).
 * Everything here is original test code; no real game's files or names are imitated. */
#include <initguid.h>
#include <windows.h>
#include <commctrl.h>
#include <mmdeviceapi.h>
#include <mmsystem.h>
#include <shlobj.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define LH_UNINSTALL_KEY "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\LanternHollow"

static unsigned fnv1a(const unsigned char *p, size_t n) {
    unsigned h = 2166136261u;
    while (n--) {
        h ^= *p++;
        h *= 16777619u;
    }
    return h;
}

/* Whole file in memory (NUL-terminated for text use); NULL if it cannot be read. */
static unsigned char *read_all(const char *path, size_t *n) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long len = ftell(f);
    fseek(f, 0, SEEK_SET);
    unsigned char *b = malloc(len + 1);
    if (b && fread(b, 1, len, f) != (size_t)len) {
        free(b);
        b = NULL;
    }
    fclose(f);
    if (b) {
        b[len] = 0;
        if (n) *n = len;
    }
    return b;
}

/* LH_AUTOCLICK: "1" clicks through every page, "cancel" presses Cancel on the License page.
 * NULL when unset (a person clicks). */
static const char *autoclick(void) {
    static char v[32];
    DWORD r = GetEnvironmentVariableA("LH_AUTOCLICK", v, sizeof v);
    return r && r < sizeof v ? v : NULL;
}

/* Appends one line to %LH_TRACE% (a test-owned log of shown pages and pressed buttons). */
static void trace(const char *fmt, ...) {
    char path[MAX_PATH];
    DWORD r = GetEnvironmentVariableA("LH_TRACE", path, sizeof path);
    if (!r || r >= sizeof path) return;
    FILE *f = fopen(path, "a");
    if (!f) return;
    va_list ap;
    va_start(ap, fmt);
    vfprintf(f, fmt, ap);
    va_end(ap);
    fputc('\n', f);
    fclose(f);
}

struct answer {
    char title[128];
    int id;
};

static DWORD WINAPI answer_thread(LPVOID p) {
    struct answer *a = p;
    for (int i = 0; i < 100; i++) {
        Sleep(400);
        HWND d = FindWindowA("#32770", a->title);
        HWND b = d ? GetDlgItem(d, a->id) : NULL;
        if (b) {
            trace("answer %s %d", a->title, a->id);
            PostMessageA(b, BM_CLICK, 0, 0);
            break;
        }
    }
    free(a);
    return 0;
}

/* In autoclick mode, presses button `id` (IDOK, IDYES...) of the message box titled `title`
 * once it appears. Call right before MessageBox. */
static void answer_later(const char *title, int id) {
    if (!autoclick()) return;
    struct answer *a = malloc(sizeof *a);
    lstrcpynA(a->title, title, sizeof a->title);
    a->id = id;
    CloseHandle(CreateThread(NULL, 0, answer_thread, a, 0, NULL));
}

/* Records what audio this process can see: waveOut devices and the default render endpoint. */
static void audio_report(FILE *f) {
    UINT n = waveOutGetNumDevs();
    HRESULT hr = CoInitializeEx(NULL, COINIT_APARTMENTTHREADED);
    HRESULT ep = E_FAIL;
    IMMDeviceEnumerator *e = NULL;
    if (SUCCEEDED(hr) || hr == RPC_E_CHANGED_MODE) {
        ep = CoCreateInstance(&CLSID_MMDeviceEnumerator, NULL, CLSCTX_ALL,
                              &IID_IMMDeviceEnumerator, (void **)&e);
        if (SUCCEEDED(ep)) {
            IMMDevice *d = NULL;
            ep = e->lpVtbl->GetDefaultAudioEndpoint(e, eRender, eConsole, &d);
            if (d) d->lpVtbl->Release(d);
            e->lpVtbl->Release(e);
        }
    }
    fprintf(f, "waveout_devices=%u\ndefault_render=%s\ndefault_render_hr=0x%08lx\n", n,
            SUCCEEDED(ep) ? "yes" : "no", (unsigned long)ep);
}

/* A short generated tone (16-bit mono PCM WAV, in memory). */
static unsigned char *make_wav(DWORD *size) {
    const DWORD rate = 22050, samples = rate / 2;
    DWORD data = samples * 2;
    *size = 44 + data;
    unsigned char *w = calloc(1, *size);
    memcpy(w, "RIFF", 4);
    *(DWORD *)(w + 4) = 36 + data;
    memcpy(w + 8, "WAVEfmt ", 8);
    *(DWORD *)(w + 16) = 16;
    *(WORD *)(w + 20) = 1;
    *(WORD *)(w + 22) = 1;
    *(DWORD *)(w + 24) = rate;
    *(DWORD *)(w + 28) = rate * 2;
    *(WORD *)(w + 32) = 2;
    *(WORD *)(w + 34) = 16;
    memcpy(w + 36, "data", 4);
    *(DWORD *)(w + 40) = data;
    short *s = (short *)(w + 44);
    for (DWORD i = 0; i < samples; i++) s[i] = ((i / 25) % 2) ? 3000 : -3000; /* ~441 Hz */
    return w;
}

/* Creates every directory of a full path (C:\a\b\c). */
static void mkdirs(const char *path) {
    char b[MAX_PATH];
    lstrcpynA(b, path, sizeof b);
    for (char *p = b + 3; *p; p++)
        if (*p == '\\') {
            *p = 0;
            CreateDirectoryA(b, NULL);
            *p = '\\';
        }
    CreateDirectoryA(b, NULL);
}

/* %APPDATA%\LanternHollow, created. */
static void appdata_dir(char *out) {
    SHGetFolderPathA(NULL, CSIDL_APPDATA, NULL, 0, out);
    lstrcatA(out, "\\LanternHollow");
    mkdirs(out);
}

/* The left banner of the wizard and the game's title art: night sky and a lantern (GDI). */
static void paint_lantern(HDC dc, RECT r) {
    HBRUSH sky = CreateSolidBrush(RGB(24, 30, 58));
    FillRect(dc, &r, sky);
    DeleteObject(sky);
    int cx = (r.left + r.right) / 2, cy = r.top + (r.bottom - r.top) * 2 / 5;
    HBRUSH glow = CreateSolidBrush(RGB(110, 84, 30));
    HBRUSH flame = CreateSolidBrush(RGB(255, 196, 64));
    HBRUSH frame = CreateSolidBrush(RGB(60, 48, 40));
    HPEN none = GetStockObject(NULL_PEN);
    HGDIOBJ op = SelectObject(dc, none);
    HGDIOBJ ob = SelectObject(dc, glow);
    Ellipse(dc, cx - 46, cy - 46, cx + 46, cy + 46);
    SelectObject(dc, frame);
    Rectangle(dc, cx - 20, cy - 34, cx + 20, cy - 26);
    Rectangle(dc, cx - 20, cy + 26, cx + 20, cy + 34);
    Rectangle(dc, cx - 3, cy - 46, cx + 3, cy - 34);
    SelectObject(dc, flame);
    Ellipse(dc, cx - 14, cy - 24, cx + 14, cy + 24);
    SelectObject(dc, ob);
    SelectObject(dc, op);
    DeleteObject(glow);
    DeleteObject(flame);
    DeleteObject(frame);
}
