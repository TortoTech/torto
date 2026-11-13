#pragma once
#include <stddef.h>
#include <stdint.h>

// The provider owns the returned token; pixels are immutable until release.
const void *torto_ios_retain_pixels(uint64_t id, const uint8_t **data, size_t *size);
void torto_ios_release_pixels(const void *token);
