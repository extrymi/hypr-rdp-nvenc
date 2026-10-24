#ifndef HYPR_NVENC_WRAPPER_H
#define HYPR_NVENC_WRAPPER_H
#include "nvEncodeAPI.h"
#include "dynlink_cuda.h"

/* Expand version macros with the C compiler; never reproduce ABI versions in Rust. */
#define HYPR_VERSION(name) static const uint32_t HYPR_##name = name
HYPR_VERSION(NV_ENCODE_API_FUNCTION_LIST_VER);
HYPR_VERSION(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER);
HYPR_VERSION(NV_ENC_CAPS_PARAM_VER);
HYPR_VERSION(NV_ENC_PRESET_CONFIG_VER);
HYPR_VERSION(NV_ENC_CONFIG_VER);
HYPR_VERSION(NV_ENC_RC_PARAMS_VER);
HYPR_VERSION(NV_ENC_INITIALIZE_PARAMS_VER);
HYPR_VERSION(NV_ENC_CREATE_INPUT_BUFFER_VER);
HYPR_VERSION(NV_ENC_CREATE_BITSTREAM_BUFFER_VER);
HYPR_VERSION(NV_ENC_LOCK_INPUT_BUFFER_VER);
HYPR_VERSION(NV_ENC_PIC_PARAMS_VER);
HYPR_VERSION(NV_ENC_LOCK_BITSTREAM_VER);
static const uint32_t HYPR_MIN_API = (NVENCAPI_MAJOR_VERSION << 4) | NVENCAPI_MINOR_VERSION;
/* C owns aggregate GUID initializers; bindgen must not emit undefined extern
 * references to header-local static constants. Return typed values instead. */
GUID hypr_nvenc_h264(void);
GUID hypr_nvenc_profile(void);
GUID hypr_nvenc_preset(void);

typedef NVENCSTATUS (NVENCAPI *HYPR_GET_MAX_VERSION)(uint32_t *);
typedef NVENCSTATUS (NVENCAPI *HYPR_CREATE_INSTANCE)(NV_ENCODE_API_FUNCTION_LIST *);
typedef tcuInit *HYPR_CU_INIT;
typedef tcuDeviceGetCount *HYPR_CU_COUNT;
typedef tcuDeviceGet *HYPR_CU_DEVICE;
typedef tcuDeviceGetUuid *HYPR_CU_UUID;
typedef tcuCtxCreate_v2 *HYPR_CU_CREATE;
typedef tcuCtxDestroy_v2 *HYPR_CU_DESTROY;
typedef struct HYPR_CUDA_FUNCTIONS {
    HYPR_CU_INIT init;
    HYPR_CU_COUNT count;
    HYPR_CU_DEVICE device;
    HYPR_CU_UUID uuid;
    HYPR_CU_CREATE create;
    HYPR_CU_DESTROY destroy;
} HYPR_CUDA_FUNCTIONS;
#endif
