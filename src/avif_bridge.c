#include <avif/avif.h>
#include <stdint.h>
#include <stdlib.h>

typedef struct ImageGuideAvifData {
    avifRWData raw;
} ImageGuideAvifData;

ImageGuideAvifData *imageguide_avif_encode(const uint8_t *pixels,
                                           uint32_t width,
                                           uint32_t height,
                                           int has_alpha,
                                           int quality,
                                           int speed,
                                           int threads) {
    const uint32_t channels = has_alpha ? 4 : 3;
    if (!pixels || !width || !height || width > UINT32_MAX / channels) {
        return NULL;
    }

    avifImage *image = avifImageCreate(width, height, 8, AVIF_PIXEL_FORMAT_YUV444);
    if (!image) {
        return NULL;
    }

    avifRGBImage rgb;
    avifRGBImageSetDefaults(&rgb, image);
    rgb.format = has_alpha ? AVIF_RGB_FORMAT_RGBA : AVIF_RGB_FORMAT_RGB;
    rgb.pixels = (uint8_t *)pixels;
    rgb.rowBytes = width * channels;
    if (avifImageRGBToYUV(image, &rgb) != AVIF_RESULT_OK) {
        avifImageDestroy(image);
        return NULL;
    }

    avifEncoder *encoder = avifEncoderCreate();
    if (!encoder) {
        avifImageDestroy(image);
        return NULL;
    }
    encoder->codecChoice = AVIF_CODEC_CHOICE_AOM;
    encoder->quality = quality;
    encoder->qualityAlpha = quality;
    encoder->speed = speed;
    encoder->maxThreads = threads;

    ImageGuideAvifData *encoded = malloc(sizeof(*encoded));
    if (!encoded) {
        avifEncoderDestroy(encoder);
        avifImageDestroy(image);
        return NULL;
    }
    encoded->raw.data = NULL;
    encoded->raw.size = 0;
    if (avifEncoderWrite(encoder, image, &encoded->raw) != AVIF_RESULT_OK) {
        avifRWDataFree(&encoded->raw);
        free(encoded);
        encoded = NULL;
    }

    avifEncoderDestroy(encoder);
    avifImageDestroy(image);
    return encoded;
}

const uint8_t *imageguide_avif_data(const ImageGuideAvifData *encoded) {
    return encoded ? encoded->raw.data : NULL;
}

size_t imageguide_avif_size(const ImageGuideAvifData *encoded) {
    return encoded ? encoded->raw.size : 0;
}

void imageguide_avif_free(ImageGuideAvifData *encoded) {
    if (encoded) {
        avifRWDataFree(&encoded->raw);
        free(encoded);
    }
}
