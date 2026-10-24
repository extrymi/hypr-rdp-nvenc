/* Independent CPU-only C implementation of the pinned foreign API contract.
 * This is original test code, not an NVIDIA/FFmpeg implementation. */
#include "wrapper.h"
#include <stdio.h>
#include <string.h>

typedef struct { uint8_t *bytes; uint32_t pitch, height; int locked; } Input;
typedef struct { uint64_t timestamp; uint32_t index; int idr, locked; } Output;
static _Thread_local char trace[16384], fail_name[64];
static _Thread_local unsigned fail_n, hits, calls;
static _Thread_local int resources, selected, mode, eos_seen;
static _Thread_local uint8_t *packet;
static _Thread_local uint32_t packet_size;

static int event(const char *name) {
    size_t n = strlen(trace);
    if (n < sizeof(trace) - 128) snprintf(trace + n, sizeof(trace) - n, "%s\n", name);
    if (strcmp(name, fail_name) == 0 && ++hits == fail_n) return 1;
    return 0;
}
void hypr_mock_reset(int new_mode, const char *failure, unsigned nth) {
    free(packet); packet = NULL; packet_size = 0;
    trace[0] = 0; snprintf(fail_name, sizeof(fail_name), "%s", failure);
    fail_n = nth; hits = 0; resources = 0; selected = -1; mode = new_mode; calls = 0; eos_seen = 0;
}
const char *hypr_mock_trace(void) { return trace; }
int hypr_mock_resources(void) { return resources; }
int hypr_mock_selected(void) { return selected; }
void hypr_mock_packet(const uint8_t *data, uint32_t length) {
    free(packet); packet = malloc(length); if (length) memcpy(packet, data, length); packet_size = length;
}

static CUresult CUDAAPI init(unsigned flags) { (void)flags; return event("cu-init") ? CUDA_ERROR_UNKNOWN : CUDA_SUCCESS; }
static CUresult CUDAAPI count(int *n) { if (event("cu-count")) return CUDA_ERROR_UNKNOWN; *n = 2; return CUDA_SUCCESS; }
static CUresult CUDAAPI device(CUdevice *d, int ordinal) { if (event("cu-device")) return CUDA_ERROR_UNKNOWN; *d = ordinal + 10; return CUDA_SUCCESS; }
static CUresult CUDAAPI uuid(CUuuid *u, CUdevice d) {
    static const uint8_t bytes[16] = {1,35,69,103,137,171,205,239,1,35,69,103,137,171,205,239};
    if (event("cu-uuid")) return CUDA_ERROR_UNKNOWN;
    memset(u, 0, sizeof(*u)); if (d == 11) memcpy(u->bytes, bytes, 16); return CUDA_SUCCESS;
}
static CUresult CUDAAPI context(CUcontext *ctx, unsigned flags, CUdevice d) {
    if (event("cu-create") || flags != CU_CTX_SCHED_BLOCKING_SYNC) return CUDA_ERROR_UNKNOWN;
    selected = d; *ctx = (CUcontext)malloc(1); resources++; return CUDA_SUCCESS;
}
static CUresult CUDAAPI context_destroy(CUcontext ctx) {
    int fail = event("cu-destroy"); free(ctx); resources--; return fail ? CUDA_ERROR_UNKNOWN : CUDA_SUCCESS;
}
static NVENCSTATUS NVENCAPI open_session(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS *p, void **session) {
    if (event("open") || p->version != NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER || p->apiVersion != NVENCAPI_VERSION
        || p->deviceType != NV_ENC_DEVICE_TYPE_CUDA || !p->device) return NV_ENC_ERR_INVALID_PARAM;
    *session = malloc(1); resources++; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI close_session(void *s) {
    int fail = event("close"); free(s); resources--; return fail ? NV_ENC_ERR_GENERIC : NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI codec_count(void *s, uint32_t *n) {
    (void)s; if (event("codec-count")) return NV_ENC_ERR_GENERIC;
    *n = mode == 11 ? 10000 : 1; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI codecs(void *s, GUID *g, uint32_t capacity, uint32_t *n) {
    (void)s; if (event("codecs") || capacity < 1) return NV_ENC_ERR_GENERIC;
    g[0] = NV_ENC_CODEC_H264_GUID; *n = 1; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI specific_count(void *s, GUID codec, uint32_t *n) {
    (void)s; (void)codec; if (event("specific-count")) return NV_ENC_ERR_GENERIC; *n = 1; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI profiles(void *s, GUID codec, GUID *g, uint32_t capacity, uint32_t *n) {
    (void)s; (void)codec; if (event("profiles") || capacity < 1) return NV_ENC_ERR_GENERIC;
    g[0] = NV_ENC_H264_PROFILE_HIGH_GUID; *n = 1; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI presets(void *s, GUID codec, GUID *g, uint32_t capacity, uint32_t *n) {
    (void)s; (void)codec; if (event("presets") || capacity < 1) return NV_ENC_ERR_GENERIC;
    g[0] = NV_ENC_PRESET_P1_GUID; *n = 1; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI formats(void *s, GUID codec, NV_ENC_BUFFER_FORMAT *f, uint32_t capacity, uint32_t *n) {
    (void)s; (void)codec; if (event("formats") || capacity < 1) return NV_ENC_ERR_GENERIC;
    f[0] = mode == 1 ? NV_ENC_BUFFER_FORMAT_ARGB : NV_ENC_BUFFER_FORMAT_NV12; *n = 1; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI caps(void *s, GUID codec, NV_ENC_CAPS_PARAM *p, int *value) {
    (void)s; (void)codec;
    if (event("caps") || p->version != NV_ENC_CAPS_PARAM_VER) return NV_ENC_ERR_INVALID_VERSION;
    switch (p->capsToQuery) {
        case NV_ENC_CAPS_WIDTH_MIN: case NV_ENC_CAPS_HEIGHT_MIN: *value = 16; break;
        case NV_ENC_CAPS_WIDTH_MAX: case NV_ENC_CAPS_HEIGHT_MAX: *value = 8192; break;
        case NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES: *value = 1 << NV_ENC_PARAMS_RC_CBR; break;
        case NV_ENC_CAPS_SUPPORT_CABAC: *value = 1; break;
        default: *value = 100000000; break;
    }
    return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI preset(void *s, GUID codec, GUID guid, NV_ENC_TUNING_INFO tuning, NV_ENC_PRESET_CONFIG *p) {
    (void)s; (void)codec; (void)guid;
    if (event("preset") || p->version != NV_ENC_PRESET_CONFIG_VER || p->presetCfg.version != NV_ENC_CONFIG_VER
        || tuning != NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY) return NV_ENC_ERR_INVALID_PARAM;
    p->presetCfg.frameIntervalP = 3; p->presetCfg.rcParams.enableLookahead = 1;
    p->presetCfg.rcParams.enableExtLookahead = 1; p->presetCfg.rcParams.lookaheadDepth = 7;
    p->presetCfg.rcParams.enableAQ = 1; p->presetCfg.rcParams.enableTemporalAQ = 1;
    return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI initialize(void *s, NV_ENC_INITIALIZE_PARAMS *p) {
    (void)s;
    if (event("initialize") || p->version != NV_ENC_INITIALIZE_PARAMS_VER || !p->encodeConfig) return NV_ENC_ERR_GENERIC;
    NV_ENC_CONFIG *c = p->encodeConfig;
    NV_ENC_CONFIG_H264 *h = &c->encodeCodecConfig.h264Config;
    if (c->version != NV_ENC_CONFIG_VER || c->rcParams.version != NV_ENC_RC_PARAMS_VER
        || c->frameIntervalP != 1 || c->rcParams.enableLookahead || c->rcParams.lookaheadDepth
        || c->rcParams.enableExtLookahead || c->rcParams.enableAQ || c->rcParams.enableTemporalAQ
        || !c->rcParams.zeroReorderDelay || c->rcParams.rateControlMode != NV_ENC_PARAMS_RC_CBR
        || p->enableEncodeAsync || !p->enablePTD || h->chromaFormatIDC != 1
        || h->inputBitDepth != NV_ENC_BIT_DEPTH_8 || h->outputBitDepth != NV_ENC_BIT_DEPTH_8
        || !h->repeatSPSPPS || h->disableSPSPPS || p->frameRateDen != 1
        || h->h264VUIParameters.videoFullRangeFlag != 1) {
        event("bad-config"); return NV_ENC_ERR_INVALID_PARAM;
    }
    return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI create_input(void *s, NV_ENC_CREATE_INPUT_BUFFER *p) {
    (void)s;
    if (event("create-input") || p->version != NV_ENC_CREATE_INPUT_BUFFER_VER || p->bufferFmt != NV_ENC_BUFFER_FORMAT_NV12) return NV_ENC_ERR_GENERIC;
    if (mode == 9) { p->inputBuffer = NULL; return NV_ENC_SUCCESS; }
    Input *i = calloc(1, sizeof(*i)); i->pitch = p->width + 32; i->height = p->height;
    i->bytes = malloc((size_t)i->pitch * i->height * 3 / 2); memset(i->bytes, 0xEE, (size_t)i->pitch * i->height * 3 / 2);
    p->inputBuffer = i; resources++; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI destroy_input(void *s, NV_ENC_INPUT_PTR input) {
    (void)s; int fail = event("destroy-input"); Input *i = input;
    if (!eos_seen) event("destroy-before-eos");
    if (i->locked) event("destroy-locked-input");
    free(i->bytes); free(i); resources--; return fail ? NV_ENC_ERR_GENERIC : NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI create_output(void *s, NV_ENC_CREATE_BITSTREAM_BUFFER *p) {
    (void)s; if (event("create-output") || p->version != NV_ENC_CREATE_BITSTREAM_BUFFER_VER) return NV_ENC_ERR_GENERIC;
    p->bitstreamBuffer = calloc(1, sizeof(Output)); resources++; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI destroy_output(void *s, NV_ENC_OUTPUT_PTR output) {
    (void)s; int fail = event("destroy-output"); Output *o = output;
    if (!eos_seen) event("destroy-before-eos");
    if (o->locked) event("destroy-locked-output");
    free(o); resources--; return fail ? NV_ENC_ERR_GENERIC : NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI lock_input(void *s, NV_ENC_LOCK_INPUT_BUFFER *p) {
    (void)s; if (event("lock-input") || p->version != NV_ENC_LOCK_INPUT_BUFFER_VER || !p->doNotWait) return NV_ENC_ERR_GENERIC;
    Input *i = p->inputBuffer; if (i->locked) return NV_ENC_ERR_INVALID_PARAM; i->locked = 1;
    p->bufferDataPtr = i->bytes; p->pitch = mode == 5 ? 1 : i->pitch; return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI unlock_input(void *s, NV_ENC_INPUT_PTR input) {
    (void)s; int fail = event("unlock-input"); Input *i = input;
    if (!i->locked) return NV_ENC_ERR_INVALID_PARAM;
    i->locked = 0;
    return fail ? NV_ENC_ERR_GENERIC : NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI encode(void *s, NV_ENC_PIC_PARAMS *p) {
    (void)s;
    if (p->encodePicFlags & NV_ENC_PIC_FLAG_EOS) { eos_seen = 1; return event("eos") ? NV_ENC_ERR_GENERIC : NV_ENC_SUCCESS; }
    if (event("encode") || p->version != NV_ENC_PIC_PARAMS_VER || p->bufferFmt != NV_ENC_BUFFER_FORMAT_NV12
        || p->pictureStruct != NV_ENC_PIC_STRUCT_FRAME) return NV_ENC_ERR_GENERIC;
    Input *i = p->inputBuffer;
    if (i->locked || p->inputPitch != i->pitch) return NV_ENC_ERR_INVALID_PARAM;
    /* Tests fill every NV12 source byte with 0x42; padding must stay untouched. */
    for (uint32_t row = 0; row < i->height * 3 / 2; row++) {
        for (uint32_t x = 0; x < i->pitch; x++) {
            if (i->bytes[(size_t)row * i->pitch + x] != (x < p->inputWidth ? 0x42 : 0xEE)) {
                event("bad-pitch-copy"); return NV_ENC_ERR_INVALID_PARAM;
            }
        }
    }
    calls++; if (mode == 7 && calls <= 2) return NV_ENC_ERR_ENCODER_BUSY;
    Output *o = p->outputBitstream; o->timestamp = p->inputTimeStamp; o->index = p->frameIdx;
    o->idr = !!(p->encodePicFlags & NV_ENC_PIC_FLAG_FORCEIDR);
    return mode == 6 ? NV_ENC_ERR_NEED_MORE_INPUT : NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI lock_output(void *s, NV_ENC_LOCK_BITSTREAM *p) {
    (void)s; if (event("lock-output") || p->version != NV_ENC_LOCK_BITSTREAM_VER || p->doNotWait) return NV_ENC_ERR_GENERIC;
    Output *o = p->outputBitstream; if (o->locked) return NV_ENC_ERR_INVALID_PARAM; o->locked = 1;
    p->bitstreamBufferPtr = mode == 4 ? NULL : packet; p->bitstreamSizeInBytes = mode == 3 ? 17000000 : packet_size;
    p->outputTimeStamp = o->timestamp + (mode == 2); p->frameIdx = o->index;
    p->pictureType = mode == 8 ? NV_ENC_PIC_TYPE_B : (o->idr ? NV_ENC_PIC_TYPE_IDR : NV_ENC_PIC_TYPE_P);
    return NV_ENC_SUCCESS;
}
static NVENCSTATUS NVENCAPI unlock_output(void *s, NV_ENC_OUTPUT_PTR output) {
    (void)s; int fail = event("unlock-output"); Output *o = output;
    if (!o->locked) return NV_ENC_ERR_INVALID_PARAM;
    o->locked = 0;
    return fail ? NV_ENC_ERR_GENERIC : NV_ENC_SUCCESS;
}
void hypr_mock_api(HYPR_CUDA_FUNCTIONS *cuda, NV_ENCODE_API_FUNCTION_LIST *enc) {
    memset(cuda, 0, sizeof(*cuda)); memset(enc, 0, sizeof(*enc));
    cuda->init = init; cuda->count = count; cuda->device = device; cuda->uuid = uuid;
    cuda->create = context; cuda->destroy = context_destroy;
    enc->version = NV_ENCODE_API_FUNCTION_LIST_VER;
    enc->nvEncOpenEncodeSessionEx = open_session; enc->nvEncDestroyEncoder = close_session;
    enc->nvEncGetEncodeGUIDCount = codec_count; enc->nvEncGetEncodeGUIDs = codecs;
    enc->nvEncGetEncodeProfileGUIDCount = specific_count; enc->nvEncGetEncodeProfileGUIDs = profiles;
    enc->nvEncGetEncodePresetCount = specific_count; enc->nvEncGetEncodePresetGUIDs = presets;
    enc->nvEncGetInputFormatCount = specific_count; enc->nvEncGetInputFormats = formats;
    enc->nvEncGetEncodeCaps = caps; enc->nvEncGetEncodePresetConfigEx = preset;
    enc->nvEncInitializeEncoder = initialize;
    enc->nvEncCreateInputBuffer = create_input; enc->nvEncDestroyInputBuffer = destroy_input;
    enc->nvEncCreateBitstreamBuffer = create_output; enc->nvEncDestroyBitstreamBuffer = destroy_output;
    enc->nvEncLockInputBuffer = lock_input; enc->nvEncUnlockInputBuffer = unlock_input;
    enc->nvEncEncodePicture = encode; enc->nvEncLockBitstream = lock_output; enc->nvEncUnlockBitstream = unlock_output;
}

/* Export the system-driver symbol names only in this CPU test fixture. */
CUresult CUDAAPI cuInit(unsigned flags) { return init(flags); }
CUresult CUDAAPI cuDeviceGetCount(int *n) { return count(n); }
CUresult CUDAAPI cuDeviceGet(CUdevice *d, int ordinal) { return device(d, ordinal); }
CUresult CUDAAPI cuDeviceGetUuid(CUuuid *u, CUdevice d) { return uuid(u, d); }
CUresult CUDAAPI cuCtxCreate_v2(CUcontext *ctx, unsigned flags, CUdevice d) { return context(ctx, flags, d); }
CUresult CUDAAPI cuCtxDestroy_v2(CUcontext ctx) { return context_destroy(ctx); }
NVENCSTATUS NVENCAPI NvEncodeAPIGetMaxSupportedVersion(uint32_t *version) {
    if (event("maximum-api")) return NV_ENC_ERR_GENERIC;
    *version = mode == 12 ? HYPR_MIN_API - 1 : HYPR_MIN_API; return NV_ENC_SUCCESS;
}
NVENCSTATUS NVENCAPI NvEncodeAPICreateInstance(NV_ENCODE_API_FUNCTION_LIST *enc) {
    if (event("create-instance") || enc->version != NV_ENCODE_API_FUNCTION_LIST_VER) return NV_ENC_ERR_INVALID_VERSION;
    HYPR_CUDA_FUNCTIONS cuda; hypr_mock_api(&cuda, enc);
    if (mode == 13) enc->nvEncUnlockBitstream = NULL;
    return NV_ENC_SUCCESS;
}
