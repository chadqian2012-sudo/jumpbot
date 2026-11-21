// OpenCV RGB2HSV_b integer conversion (Apache-2.0, see THIRD_PARTY_NOTICES.md).
// Build CUDA PTX with nvcc -x cu -ptx -arch=compute_52 masks_kernel.cl -o masks_kernel.ptx.
#ifdef __CUDACC__
#define GLOBAL
#define CONST
#define KERNEL extern "C" __global__
#define INLINE static __device__ __forceinline__
#define INDEX (blockIdx.x * blockDim.x + threadIdx.x)
#else
#define GLOBAL __global
#define CONST __constant
#define KERNEL __kernel
#define INLINE static inline
#define INDEX get_global_id(0)
#endif

typedef unsigned char Byte;
typedef struct { int h, s, v; } Hsv;

INLINE Hsv hsv(GLOBAL const Byte *p, CONST const int *table) {
    int b = p[0], g = p[1], r = p[2];
    int v = max(b, max(g, r)), d = v - min(b, min(g, r));
    int raw = v == r ? g - b : v == g ? b - r + 2*d : r - g + 4*d;
    int h = (raw * table[256 + d] + 2048) >> 12;
    Hsv c = {h < 0 ? h + 180 : h, (d * table[v] + 2048) >> 12, v};
    return c;
}

KERNEL void jumpbot_masks(GLOBAL const Byte *bgr, GLOBAL const Byte *bg,
                          CONST const int *table, GLOBAL Byte *out,
                          int n, int width, int top, int bottom) {
    unsigned int i = INDEX;
    if (i >= (unsigned int)n) return;
    int row = i / width;
    if (row < top || row >= bottom) {
        out[i] = out[n + i] = 0;
        return;
    }
    GLOBAL const Byte *p = bgr + 3u * i, *base = bg + 3u * row;
    Hsv c = hsv(p, table), a = hsv(base, table);
    CONST const int *t = table + 512;
    int diff = abs((int)p[0] - base[0]) + abs((int)p[1] - base[1]) + abs((int)p[2] - base[2]);
    int block = a.v - c.v > t[1] || (abs(c.h - a.h) > t[2] && c.s > t[3])
        || c.s - a.s > t[4] || a.s - c.s > t[5]
        || (c.s < t[6] && c.v > t[7]) || diff > t[0]
        || (t[13] && c.v - a.v > t[8] && c.s <= a.s);
    int piece = ((c.s > t[9] && c.v < t[10]) || (c.v < 110 && c.s > 25))
        && c.h >= t[11] && c.h <= t[12];
    out[i] = piece ? 255 : 0;
    out[n + i] = block ? 255 : 0;
}
