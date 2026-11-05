#version 440

layout(location = 0) out vec4 fragColor;
layout(binding = 1) uniform sampler2D pattern;

// Copies one texel per pixel. texelFetch does no filtering, so a unorm or
// float texel reaches the render target with the same value. gl_FragCoord
// has a top-left origin on D3D, Metal and Vulkan.
void main()
{
    ivec2 p = ivec2(gl_FragCoord.xy);
    ivec2 size = textureSize(pattern, 0);
    fragColor = all(lessThan(p, size)) ? texelFetch(pattern, p, 0) : vec4(0.0, 0.0, 0.0, 1.0);
}
