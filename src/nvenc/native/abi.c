#include "wrapper.h"
#include <stddef.h>

GUID hypr_nvenc_h264(void) { return NV_ENC_CODEC_H264_GUID; }
GUID hypr_nvenc_profile(void) { return NV_ENC_H264_PROFILE_HIGH_GUID; }
GUID hypr_nvenc_preset(void) { return NV_ENC_PRESET_P1_GUID; }

size_t hypr_c_sizeof_config(void) { return sizeof(NV_ENC_CONFIG); }
size_t hypr_c_sizeof_pic(void) { return sizeof(NV_ENC_PIC_PARAMS); }
size_t hypr_c_sizeof_api(void) { return sizeof(NV_ENCODE_API_FUNCTION_LIST); }
size_t hypr_c_offset_output(void) { return offsetof(NV_ENC_LOCK_BITSTREAM, bitstreamBufferPtr); }
uint32_t hypr_c_bitfield(NV_ENC_RC_PARAMS *p) {
    uint32_t result = p->zeroReorderDelay | (p->enableLookahead << 1);
    p->enableAQ = 1;
    return result;
}
