#include "alpha_correction.hlsl"

cbuffer GlobalParams: register(b0) {
    float4 gamma_ratios;
    // Rows of the linear `sRGB -> monitor gamut` matrix, used by wide_present
    // when color_output_mode == 1.
    float4 color_matrix_r0;
    float4 color_matrix_r1;
    float4 color_matrix_r2;
    float2 global_viewport_size;
    float grayscale_enhanced_contrast;
    float subpixel_enhanced_contrast;
    uint is_bgr;
    // 0: linear scRGB into an fp16 swap chain (advanced color displays).
    // 1: display-referred sRGB-encoded values into an 8-bit swap chain.
    uint color_output_mode;
    uint2 global_pad;
};

cbuffer BatchParams: register(b1) {
    uint batch_start_index;
    uint3 batch_pad;
};

Texture2D<float4> t_sprite: register(t0);
SamplerState s_sprite: register(s0);

struct SubpixelSpriteFragmentOutput {
    float4 foreground : SV_Target0;
    float4 alpha : SV_Target1;
};

struct Bounds {
    float2 origin;
    float2 size;
};

struct Corners {
    float top_left;
    float top_right;
    float bottom_right;
    float bottom_left;
};

struct ContentMask {
    Bounds bounds;
    Corners corner_radii;
    float corner_smoothing;
    float pad;
};

struct Edges {
    float top;
    float right;
    float bottom;
    float left;
};

struct Hsla {
    float h;
    float s;
    float l;
    float a;
};

struct LinearColorStop {
    Hsla color;
    float percentage;
};

struct Background {
    // 0u is Solid
    // 1u is LinearGradient
    // 2u is PatternSlash
    uint tag;
    // 0u is sRGB linear color
    // 1u is Oklab color
    uint color_space;
    Hsla solid;
    float gradient_angle_or_pattern_height;
    LinearColorStop colors[2];
    uint pad;
};

struct GradientColor {
  float4 solid;
  float4 color0;
  float4 color1;
};

struct AtlasTextureId {
    uint index;
    uint kind;
};

struct AtlasBounds {
    int2 origin;
    int2 size;
};

struct AtlasTile {
    AtlasTextureId texture_id;
    uint tile_id;
    uint padding;
    AtlasBounds bounds;
};

struct TransformationMatrix {
    float2x2 rotation_scale;
    float2 translation;
};

static const float M_PI_F = 3.141592653f;
static const float3 GRAYSCALE_FACTORS = float3(0.2126f, 0.7152f, 0.0722f);

float4 to_device_position_impl(float2 position) {
    float2 device_position = position / global_viewport_size * float2(2.0, -2.0) + float2(-1.0, 1.0);
    return float4(device_position, 0., 1.);
}

float4 to_device_position(float2 unit_vertex, Bounds bounds) {
    float2 position = unit_vertex * bounds.size + bounds.origin;
    return to_device_position_impl(position);
}

float4 distance_from_clip_rect_impl(float2 position, Bounds clip_bounds) {
    float2 tl = position - clip_bounds.origin;
    float2 br = clip_bounds.origin + clip_bounds.size - position;
    return float4(tl.x, br.x, tl.y, br.y);
}

float4 distance_from_clip_rect(float2 unit_vertex, Bounds bounds, Bounds clip_bounds) {
    float2 position = unit_vertex * bounds.size + bounds.origin;
    return distance_from_clip_rect_impl(position, clip_bounds);
}

float4 distance_from_clip_rect_transformed(float2 unit_vertex, Bounds bounds, Bounds clip_bounds, TransformationMatrix transformation) {
    float2 position = unit_vertex * bounds.size + bounds.origin;
    float2 transformed = mul(position, transformation.rotation_scale) + transformation.translation;
    return distance_from_clip_rect_impl(transformed, clip_bounds);
}

// Convert linear RGB to sRGB
float3 linear_to_srgb(float3 color) {
    return pow(color, float3(1.0 / 2.2, 1.0 / 2.2, 1.0 / 2.2));
}

// Convert sRGB to linear RGB
float3 srgb_to_linear(float3 color) {
    return pow(color, float3(2.2, 2.2, 2.2));
}

/// Hsla to linear RGBA conversion.
float4 hsla_to_rgba(Hsla hsla) {
    float h = hsla.h * 6.0; // Now, it's an angle but scaled in [0, 6) range
    float s = hsla.s;
    float l = hsla.l;
    float a = hsla.a;

    float c = (1.0 - abs(2.0 * l - 1.0)) * s;
    float x = c * (1.0 - abs(fmod(h, 2.0) - 1.0));
    float m = l - c / 2.0;

    float r = 0.0;
    float g = 0.0;
    float b = 0.0;

    if (h >= 0.0 && h < 1.0) {
        r = c;
        g = x;
        b = 0.0;
    } else if (h >= 1.0 && h < 2.0) {
        r = x;
        g = c;
        b = 0.0;
    } else if (h >= 2.0 && h < 3.0) {
        r = 0.0;
        g = c;
        b = x;
    } else if (h >= 3.0 && h < 4.0) {
        r = 0.0;
        g = x;
        b = c;
    } else if (h >= 4.0 && h < 5.0) {
        r = x;
        g = 0.0;
        b = c;
    } else {
        r = c;
        g = 0.0;
        b = x;
    }

    float4 rgba;
    rgba.x = (r + m);
    rgba.y = (g + m);
    rgba.z = (b + m);
    rgba.w = a;
    return rgba;
}

// Converts a sRGB color to the Oklab color space.
// Reference: https://bottosson.github.io/posts/oklab/#converting-from-linear-srgb-to-oklab
float4 srgb_to_oklab(float4 color) {
    // Convert non-linear sRGB to linear sRGB
    color = float4(srgb_to_linear(color.rgb), color.a);

    float l = 0.4122214708 * color.r + 0.5363325363 * color.g + 0.0514459929 * color.b;
    float m = 0.2119034982 * color.r + 0.6806995451 * color.g + 0.1073969566 * color.b;
    float s = 0.0883024619 * color.r + 0.2817188376 * color.g + 0.6299787005 * color.b;

    float l_ = pow(l, 1.0/3.0);
    float m_ = pow(m, 1.0/3.0);
    float s_ = pow(s, 1.0/3.0);

    return float4(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
        color.a
    );
}

// Converts an Oklab color to the sRGB color space.
float4 oklab_to_srgb(float4 color) {
    float l_ = color.r + 0.3963377774 * color.g + 0.2158037573 * color.b;
    float m_ = color.r - 0.1055613458 * color.g - 0.0638541728 * color.b;
    float s_ = color.r - 0.0894841775 * color.g - 1.2914855480 * color.b;

    float l = l_ * l_ * l_;
    float m = m_ * m_ * m_;
    float s = s_ * s_ * s_;

    float3 linear_rgb = float3(
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s
    );

    // Convert linear sRGB to non-linear sRGB
    return float4(linear_to_srgb(linear_rgb), color.a);
}

// This approximates the error function, needed for the gaussian integral
float2 erf(float2 x) {
    float2 s = sign(x);
    float2 a = abs(x);
    x = 1. + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    x *= x;
    return s - s / (x * x);
}

float blur_along_x(float x, float y, float sigma, float corner, float2 half_size) {
    float delta = min(half_size.y - corner - abs(y), 0.);
    float curved = half_size.x - corner + sqrt(max(0., corner * corner - delta * delta));
    float2 integral = 0.5 + 0.5 * erf((x + float2(-curved, curved)) * (sqrt(0.5) / sigma));
    return integral.y - integral.x;
}

// A standard gaussian function, used for weighting samples
float gaussian(float x, float sigma) {
    return exp(-(x * x) / (2. * sigma * sigma)) / (sqrt(2. * M_PI_F) * sigma);
}

float4 over(float4 below, float4 above) {
    float4 result;
    float alpha = above.a + below.a * (1.0 - above.a);
    result.rgb = (above.rgb * above.a + below.rgb * below.a * (1.0 - above.a)) / alpha;
    result.a = alpha;
    return result;
}

float2 to_tile_position(float2 unit_vertex, AtlasTile tile) {
    float2 atlas_size;
    t_sprite.GetDimensions(atlas_size.x, atlas_size.y);
    return (float2(tile.bounds.origin) + unit_vertex * float2(tile.bounds.size)) / atlas_size;
}

// Selects corner radius based on quadrant.
float pick_corner_radius(float2 center_to_point, Corners corner_radii) {
    if (center_to_point.x < 0.) {
        if (center_to_point.y < 0.) {
            return corner_radii.top_left;
        } else {
            return corner_radii.bottom_left;
        }
    } else {
        if (center_to_point.y < 0.) {
            return corner_radii.top_right;
        } else {
            return corner_radii.bottom_right;
        }
    }
}

float4 to_device_position_transformed(float2 unit_vertex, Bounds bounds,
                                      TransformationMatrix transformation) {
    float2 position = unit_vertex * bounds.size + bounds.origin;
    float2 transformed = mul(position, transformation.rotation_scale) + transformation.translation;
    float2 device_position = transformed / global_viewport_size * float2(2.0, -2.0) + float2(-1.0, 1.0);
    return float4(device_position, 0.0, 1.0);
}

// Implementation of quad signed distance field
float quad_sdf_impl(float2 corner_center_to_point, float corner_radius) {
    if (corner_radius == 0.0) {
        // Fast path for unrounded corners
        return max(corner_center_to_point.x, corner_center_to_point.y);
    } else {
        // Signed distance of the point from a quad that is inset by corner_radius
        // It is negative inside this quad, and positive outside
        float signed_distance_to_inset_quad =
            // 0 inside the inset quad, and positive outside
            length(max(float2(0.0, 0.0), corner_center_to_point)) +
            // 0 outside the inset quad, and negative inside
            min(0.0, max(corner_center_to_point.x, corner_center_to_point.y));

        return signed_distance_to_inset_quad - corner_radius;
    }
}

float quad_sdf(float2 pt, Bounds bounds, Corners corner_radii) {
    float2 half_size = bounds.size / 2.;
    float2 center = bounds.origin + half_size;
    float2 center_to_point = pt - center;
    float corner_radius = pick_corner_radius(center_to_point, corner_radii);
    float2 corner_to_point = abs(center_to_point) - half_size;
    float2 corner_center_to_point = corner_to_point + corner_radius;
    return quad_sdf_impl(corner_center_to_point, corner_radius);
}

// --- corner smoothing --- //
// Derived from figma-squircle by Tien Pham (MIT), see THIRD_PARTY_NOTICES.md.

struct SmoothCorners {
    float4 horizontal_reaches;
    float4 vertical_reaches;
    float4 factors;
};

struct SmoothAxis {
    float a;
    float b;
    float p;
};

struct SmoothCornerParams {
    float radius;
    float smoothing;
    float arc_sweep;
    float c;
    float d;
    SmoothAxis horizontal;
    SmoothAxis vertical;
};

struct SmoothCubicPoint {
    float2 pt;
    float2 tangent;
    float distance;
    float path_t;
};

struct SmoothSample {
    float distance;
    float2 normal;
    float path_t;
    uint segment;
    uint corner;
};

static const float SMOOTH_EPSILON = 0.000001;
static const uint SMOOTH_SEGMENT_STRAIGHT = 0;
static const uint SMOOTH_SEGMENT_FIRST_CUBIC = 1;
static const uint SMOOTH_SEGMENT_ARC = 2;
static const uint SMOOTH_SEGMENT_SECOND_CUBIC = 3;
static const uint SMOOTH_NO_CORNER = 4;

float4 corners_to_float4(Corners corners) {
    return float4(corners.top_left, corners.top_right, corners.bottom_right,
                  corners.bottom_left);
}

float2 smooth_split_side(float side_length, float first_radius, float second_radius) {
    float total_radius = first_radius + second_radius;
    if (total_radius == 0.0) {
        return (float2)0.0;
    }
    float first_budget = side_length * first_radius / total_radius;
    return float2(first_budget, side_length - first_budget);
}

float4 smooth_factors(float corner_smoothing) {
    float smoothing = clamp(corner_smoothing, 0.0, 1.0);
    float arc_sweep = 0.5 * M_PI_F * (1.0 - smoothing);
    float beta = (M_PI_F / 4.0) * smoothing;
    float join_handle_factor = tan(0.5 * beta);
    return float4(smoothing, sin(0.5 * arc_sweep) * sqrt(2.0),
                  join_handle_factor * cos(beta), join_handle_factor * sin(beta));
}

SmoothCorners smooth_prepare(float2 size, float4 radii_values, float corner_smoothing) {
    float radii[4] = {max(radii_values.x, 0.0), max(radii_values.y, 0.0),
                      max(radii_values.z, 0.0), max(radii_values.w, 0.0)};
    float budgets[4] = {-1.0, -1.0, -1.0, -1.0};
    uint order[4] = {0, 1, 3, 2};
    static const uint horizontal_neighbors[4] = {1, 0, 3, 2};
    static const uint vertical_neighbors[4] = {3, 2, 1, 0};

    for (uint sweep = 0; sweep < 3; sweep++) {
        for (uint index = 0; index < 3 - sweep; index++) {
            if (radii[order[index]] < radii[order[index + 1]]) {
                uint swap = order[index];
                order[index] = order[index + 1];
                order[index + 1] = swap;
            }
        }
    }

    for (uint rank = 0; rank < 4; rank++) {
        uint corner = order[rank];
        float radius = radii[corner];
        uint horizontal_neighbor = horizontal_neighbors[corner];
        uint vertical_neighbor = vertical_neighbors[corner];

        float horizontal_radius = radii[horizontal_neighbor];
        float horizontal_budget = 0.0;
        if (radius != 0.0 || horizontal_radius != 0.0) {
            if (budgets[horizontal_neighbor] >= 0.0) {
                horizontal_budget = size.x - budgets[horizontal_neighbor];
            } else {
                horizontal_budget = size.x * radius / (radius + horizontal_radius);
            }
        }

        float vertical_radius = radii[vertical_neighbor];
        float vertical_budget = 0.0;
        if (radius != 0.0 || vertical_radius != 0.0) {
            if (budgets[vertical_neighbor] >= 0.0) {
                vertical_budget = size.y - budgets[vertical_neighbor];
            } else {
                vertical_budget = size.y * radius / (radius + vertical_radius);
            }
        }

        float budget = max(0.0, min(horizontal_budget, vertical_budget));
        budgets[corner] = budget;
        radii[corner] = min(radius, budget);
    }

    float2 top = smooth_split_side(size.x, radii[0], radii[1]);
    float2 bottom = smooth_split_side(size.x, radii[3], radii[2]);
    float2 left = smooth_split_side(size.y, radii[0], radii[3]);
    float2 right = smooth_split_side(size.y, radii[1], radii[2]);
    float4 horizontal_budgets = max(float4(top.x, top.y, bottom.y, bottom.x), (float4)0.0);
    float4 vertical_budgets = max(float4(left.x, right.x, right.y, left.y), (float4)0.0);
    float4 clamped_radii =
        min(max(radii_values, (float4)0.0), min(horizontal_budgets, vertical_budgets));
    float4 reaches = clamped_radii * (1.0 + clamp(corner_smoothing, 0.0, 1.0));

    SmoothCorners prepared = (SmoothCorners)0;
    prepared.horizontal_reaches = min(reaches, horizontal_budgets);
    prepared.vertical_reaches = min(reaches, vertical_budgets);
    prepared.factors = smooth_factors(corner_smoothing);
    return prepared;
}

SmoothCornerParams smooth_corner_params(float corner_radius, float horizontal_reach,
                                        float vertical_reach, float4 factors) {
    float safe_horizontal_reach = max(horizontal_reach, 0.0);
    float safe_vertical_reach = max(vertical_reach, 0.0);
    float radius =
        min(max(corner_radius, 0.0), min(safe_horizontal_reach, safe_vertical_reach));
    float smoothing = radius > SMOOTH_EPSILON ? factors.x : 0.0;
    float desired_reach = radius * (1.0 + smoothing);
    float arc_delta = factors.y * radius;
    float c = factors.z * radius;
    float d = factors.w * radius;
    float core_length = arc_delta + c + d;
    float ideal_b = max((desired_reach - core_length) / 3.0, 0.0);
    float horizontal_available = max(safe_horizontal_reach - core_length, 0.0);
    float vertical_available = max(safe_vertical_reach - core_length, 0.0);
    float shared_b = min(ideal_b, min(horizontal_available * (5.0 / 6.0),
                                      vertical_available * (5.0 / 6.0)));

    SmoothCornerParams params = (SmoothCornerParams)0;
    params.radius = radius;
    params.smoothing = smoothing;
    params.arc_sweep = 0.5 * M_PI_F * (1.0 - smoothing);
    params.c = c;
    params.d = d;
    params.horizontal.a = horizontal_available - shared_b;
    params.horizontal.b = shared_b;
    params.horizontal.p = safe_horizontal_reach;
    params.vertical.a = vertical_available - shared_b;
    params.vertical.b = shared_b;
    params.vertical.p = safe_vertical_reach;
    return params;
}

float2 smooth_cubic_point(SmoothAxis axis, float c, float d, float t) {
    float x1 = 3.0 * axis.a;
    float x2 = 3.0 * (axis.b - axis.a);
    float x3 = axis.a - 2.0 * axis.b + c;
    return float2(t * (x1 + t * (x2 + t * x3)), d * t * t * t);
}

float2 smooth_cubic_derivative(SmoothAxis axis, float c, float d, float t) {
    float x1 = 3.0 * axis.a;
    float x2 = 3.0 * (axis.b - axis.a);
    float x3 = axis.a - 2.0 * axis.b + c;
    return float2(x1 + 2.0 * x2 * t + 3.0 * x3 * t * t, 3.0 * d * t * t);
}

float2 smooth_cubic_second_derivative(SmoothAxis axis, float c, float d, float t) {
    float x2 = 3.0 * (axis.b - axis.a);
    float x3 = axis.a - 2.0 * axis.b + c;
    return float2(2.0 * x2 + 6.0 * x3 * t, 6.0 * d * t);
}

SmoothCubicPoint smooth_closest_cubic(float2 pt, SmoothAxis axis, float c, float d) {
    float y_seed = pow(clamp(pt.y / max(d, 0.00001), 0.0, 1.0), 1.0 / 3.0);
    float2 chord = smooth_cubic_point(axis, c, d, 1.0);
    float chord_seed =
        clamp(dot(pt, chord) / max(dot(chord, chord), SMOOTH_EPSILON), 0.0, 1.0);
    float2 y_delta = smooth_cubic_point(axis, c, d, y_seed) - pt;
    float2 chord_delta = smooth_cubic_point(axis, c, d, chord_seed) - pt;
    float t = dot(chord_delta, chord_delta) < dot(y_delta, y_delta) ? chord_seed : y_seed;

    for (uint iteration = 0; iteration < 4; iteration++) {
        float2 curve_point = smooth_cubic_point(axis, c, d, t);
        float2 tangent = smooth_cubic_derivative(axis, c, d, t);
        float2 second_derivative = smooth_cubic_second_derivative(axis, c, d, t);
        float2 delta = curve_point - pt;
        float denominator = dot(tangent, tangent) + dot(delta, second_derivative);
        if (abs(denominator) > SMOOTH_EPSILON) {
            t = clamp(t - dot(delta, tangent) / denominator, 0.0, 1.0);
        }
    }

    SmoothCubicPoint closest = (SmoothCubicPoint)0;
    closest.path_t = t;
    closest.pt = smooth_cubic_point(axis, c, d, t);
    closest.distance = length(pt - closest.pt);

    float start_distance = length(pt);
    if (start_distance < closest.distance) {
        closest.path_t = 0.0;
        closest.pt = (float2)0.0;
        closest.distance = start_distance;
    }

    float end_distance = length(pt - chord);
    if (end_distance < closest.distance) {
        closest.path_t = 1.0;
        closest.pt = chord;
        closest.distance = end_distance;
    }

    closest.tangent = smooth_cubic_derivative(axis, c, d, closest.path_t);
    return closest;
}

float smooth_signed_distance(float2 delta, float2 normal, float distance) {
    return dot(delta, normal) >= 0.0 ? distance : -distance;
}

float2 smooth_unfold_normal(float2 normal, bool mirrored) {
    return mirrored ? float2(-normal.y, normal.x) : float2(normal.x, -normal.y);
}

SmoothSample smooth_corner_sample(float2 corner_to_point, SmoothCornerParams params) {
    SmoothSample sample = (SmoothSample)0;
    sample.corner = SMOOTH_NO_CORNER;
    float2 z = corner_to_point + float2(params.horizontal.p, params.vertical.p);

    if (params.radius <= SMOOTH_EPSILON || z.x <= 0.0 || z.y <= 0.0) {
        sample.distance = max(corner_to_point.x, corner_to_point.y);
        sample.normal =
            corner_to_point.x > corner_to_point.y ? float2(1.0, 0.0) : float2(0.0, 1.0);
        sample.path_t = 0.0;
        sample.segment = SMOOTH_SEGMENT_STRAIGHT;
        return sample;
    }

    float2 horizontal_point = float2(z.x, params.vertical.p - z.y);
    float2 vertical_point = float2(z.y, params.horizontal.p - z.x);
    float2 circle_center = float2(params.horizontal.p - params.radius, params.radius);
    float2 join = float2(params.horizontal.a + params.horizontal.b + params.c, params.d);
    float2 start_direction = (join - circle_center) / params.radius;
    float2 to_point = horizontal_point - circle_center;
    float to_point_length = length(to_point);
    float2 point_direction = to_point_length > SMOOTH_EPSILON
                                 ? to_point / max(to_point_length, SMOOTH_EPSILON)
                                 : start_direction;

    float cross_z =
        start_direction.x * point_direction.y - start_direction.y * point_direction.x;
    float arc_angle =
        clamp(atan2(cross_z, dot(start_direction, point_direction)), 0.0, params.arc_sweep);
    float arc_sine = sin(arc_angle);
    float arc_cosine = cos(arc_angle);
    float2 arc_normal =
        float2(arc_cosine * start_direction.x - arc_sine * start_direction.y,
               arc_sine * start_direction.x + arc_cosine * start_direction.y);
    float2 arc_delta = horizontal_point - (circle_center + params.radius * arc_normal);

    sample.distance = smooth_signed_distance(arc_delta, arc_normal, length(arc_delta));
    sample.normal = smooth_unfold_normal(arc_normal, false);
    sample.path_t = params.arc_sweep > SMOOTH_EPSILON
                        ? arc_angle / max(params.arc_sweep, SMOOTH_EPSILON)
                        : 0.0;
    sample.segment = SMOOTH_SEGMENT_ARC;

    if (params.smoothing > SMOOTH_EPSILON) {
        float2 horizontal_bounds_delta =
            max((float2)0.0, max(-horizontal_point, horizontal_point - join));
        if (dot(horizontal_bounds_delta, horizontal_bounds_delta) <=
            sample.distance * sample.distance * 1.000001 + SMOOTH_EPSILON) {
            SmoothCubicPoint cubic =
                smooth_closest_cubic(horizontal_point, params.horizontal, params.c, params.d);
            float2 normal = normalize(float2(cubic.tangent.y, -cubic.tangent.x));
            float distance =
                smooth_signed_distance(horizontal_point - cubic.pt, normal, cubic.distance);
            if (abs(distance) <= abs(sample.distance)) {
                sample.distance = distance;
                sample.normal = smooth_unfold_normal(normal, false);
                sample.path_t = cubic.path_t;
                sample.segment = SMOOTH_SEGMENT_FIRST_CUBIC;
            }
        }

        float2 vertical_join =
            float2(params.vertical.a + params.vertical.b + params.c, params.d);
        float2 vertical_bounds_delta =
            max((float2)0.0, max(-vertical_point, vertical_point - vertical_join));
        if (dot(vertical_bounds_delta, vertical_bounds_delta) <=
            sample.distance * sample.distance * 1.000001 + SMOOTH_EPSILON) {
            SmoothCubicPoint cubic =
                smooth_closest_cubic(vertical_point, params.vertical, params.c, params.d);
            float2 normal = normalize(float2(cubic.tangent.y, -cubic.tangent.x));
            float distance =
                smooth_signed_distance(vertical_point - cubic.pt, normal, cubic.distance);
            if (abs(distance) <= abs(sample.distance)) {
                sample.distance = distance;
                sample.normal = smooth_unfold_normal(normal, true);
                sample.path_t = cubic.path_t;
                sample.segment = SMOOTH_SEGMENT_SECOND_CUBIC;
            }
        }
    }

    return sample;
}

float smooth_cubic_length(SmoothCornerParams params, SmoothAxis axis, float end_t) {
    float half_t = clamp(end_t, 0.0, 1.0) / 2.0;
    if (half_t <= 0.0 || params.smoothing <= 0.0) {
        return 0.0;
    }
    float offset1 = half_t * 0.5384693101;
    float offset2 = half_t * 0.9061798459;
    float speed0 = length(smooth_cubic_derivative(axis, params.c, params.d, half_t));
    float speed1 =
        length(smooth_cubic_derivative(axis, params.c, params.d, half_t - offset1)) +
        length(smooth_cubic_derivative(axis, params.c, params.d, half_t + offset1));
    float speed2 =
        length(smooth_cubic_derivative(axis, params.c, params.d, half_t - offset2)) +
        length(smooth_cubic_derivative(axis, params.c, params.d, half_t + offset2));
    return half_t * (0.5688888889 * speed0 + 0.4786286705 * speed1 + 0.2369268851 * speed2);
}

float smooth_corner_length(SmoothCornerParams params) {
    return smooth_cubic_length(params, params.horizontal, 1.0) +
           params.radius * params.arc_sweep +
           smooth_cubic_length(params, params.vertical, 1.0);
}

float smooth_corner_progress(SmoothCornerParams params, SmoothSample sample,
                             float total_length) {
    if (sample.segment == SMOOTH_SEGMENT_FIRST_CUBIC) {
        return smooth_cubic_length(params, params.horizontal, sample.path_t);
    }
    if (sample.segment == SMOOTH_SEGMENT_ARC) {
        return smooth_cubic_length(params, params.horizontal, 1.0) +
               params.radius * params.arc_sweep * sample.path_t;
    }
    if (sample.segment == SMOOTH_SEGMENT_SECOND_CUBIC) {
        return total_length - smooth_cubic_length(params, params.vertical, sample.path_t);
    }
    return 0.0;
}

bool smooth_is_corner_candidate(float2 pt, float2 size, float horizontal_extent,
                                float vertical_extent, uint corner) {
    if (horizontal_extent <= SMOOTH_EPSILON || vertical_extent <= SMOOTH_EPSILON) {
        return false;
    }
    if (corner == 0) {
        return pt.x <= horizontal_extent && pt.y <= vertical_extent;
    }
    if (corner == 1) {
        return size.x - pt.x <= horizontal_extent && pt.y <= vertical_extent;
    }
    if (corner == 2) {
        return size.x - pt.x <= horizontal_extent && size.y - pt.y <= vertical_extent;
    }
    return pt.x <= horizontal_extent && size.y - pt.y <= vertical_extent;
}

bool smooth_has_corner_candidate(float2 pt, float2 size, SmoothCorners prepared) {
    for (uint corner = 0; corner < 4; corner++) {
        if (smooth_is_corner_candidate(pt, size, prepared.horizontal_reaches[corner],
                                       prepared.vertical_reaches[corner], corner)) {
            return true;
        }
    }
    return false;
}

float2 smooth_corner_to_point(float2 pt, float2 size, uint corner) {
    if (corner == 0) {
        return -pt;
    }
    if (corner == 1) {
        return float2(pt.x - size.x, -pt.y);
    }
    if (corner == 2) {
        return pt - size;
    }
    return float2(-pt.x, pt.y - size.y);
}

float2 smooth_orient_normal(float2 normal, uint corner) {
    if (corner == 0) {
        return -normal;
    }
    if (corner == 1) {
        return float2(normal.x, -normal.y);
    }
    if (corner == 2) {
        return normal;
    }
    return float2(-normal.x, normal.y);
}

SmoothSample smooth_straight_sample(float2 pt, float2 size, SmoothCorners prepared) {
    float4 h = prepared.horizontal_reaches;
    float4 v = prepared.vertical_reaches;

    float2 nearest_delta = pt - float2(clamp(pt.x, h.x, max(h.x, size.x - h.y)), 0.0);
    float2 nearest_normal = float2(0.0, -1.0);
    float nearest_distance_squared = dot(nearest_delta, nearest_delta);

    float2 candidate_delta =
        pt - float2(size.x, clamp(pt.y, v.y, max(v.y, size.y - v.z)));
    float candidate_distance_squared = dot(candidate_delta, candidate_delta);
    if (candidate_distance_squared < nearest_distance_squared) {
        nearest_delta = candidate_delta;
        nearest_normal = float2(1.0, 0.0);
        nearest_distance_squared = candidate_distance_squared;
    }

    candidate_delta = pt - float2(clamp(pt.x, h.w, max(h.w, size.x - h.z)), size.y);
    candidate_distance_squared = dot(candidate_delta, candidate_delta);
    if (candidate_distance_squared < nearest_distance_squared) {
        nearest_delta = candidate_delta;
        nearest_normal = float2(0.0, 1.0);
        nearest_distance_squared = candidate_distance_squared;
    }

    candidate_delta = pt - float2(0.0, clamp(pt.y, v.x, max(v.x, size.y - v.w)));
    candidate_distance_squared = dot(candidate_delta, candidate_delta);
    if (candidate_distance_squared < nearest_distance_squared) {
        nearest_delta = candidate_delta;
        nearest_normal = float2(-1.0, 0.0);
        nearest_distance_squared = candidate_distance_squared;
    }

    SmoothSample sample = (SmoothSample)0;
    sample.distance = smooth_signed_distance(nearest_delta, nearest_normal,
                                             sqrt(nearest_distance_squared));
    sample.normal = nearest_normal;
    sample.path_t = 0.0;
    sample.segment = SMOOTH_SEGMENT_STRAIGHT;
    sample.corner = SMOOTH_NO_CORNER;
    return sample;
}

// `pt` is relative to the rectangle's origin.
SmoothSample smooth_rect_sample(float2 pt, float2 size, float4 radii,
                                SmoothCorners prepared) {
    SmoothSample result = smooth_straight_sample(pt, size, prepared);
    if (!smooth_has_corner_candidate(pt, size, prepared)) {
        return result;
    }

    for (uint corner = 0; corner < 4; corner++) {
        if (smooth_is_corner_candidate(pt, size, prepared.horizontal_reaches[corner],
                                       prepared.vertical_reaches[corner], corner)) {
            SmoothCornerParams params = smooth_corner_params(
                radii[corner], prepared.horizontal_reaches[corner],
                prepared.vertical_reaches[corner], prepared.factors);
            if (params.radius > SMOOTH_EPSILON) {
                SmoothSample candidate =
                    smooth_corner_sample(smooth_corner_to_point(pt, size, corner), params);
                candidate.normal = smooth_orient_normal(candidate.normal, corner);
                candidate.corner = corner;
                if (abs(candidate.distance) <= abs(result.distance)) {
                    result = candidate;
                }
            }
        }
    }

    return result;
}

float smooth_rect_sdf(float2 pt, Bounds bounds,
                      Corners corner_radii, float corner_smoothing) {
    float4 radii = corners_to_float4(corner_radii);
    float2 origin = bounds.origin;
    float2 size = bounds.size;
    SmoothCorners prepared = smooth_prepare(size, radii, corner_smoothing);
    return smooth_rect_sample(pt - origin, size, radii, prepared).distance;
}

float rounded_rect_sdf(float2 pt, Bounds bounds,
                       Corners corner_radii, float corner_smoothing) {
    if (corner_smoothing <= 0.0) {
        return quad_sdf(pt, bounds, corner_radii);
    }
    return smooth_rect_sdf(pt, bounds, corner_radii, corner_smoothing);
}

float content_mask_alpha(float2 pt, ContentMask mask) {
    if (mask.corner_radii.top_left == 0.0 && mask.corner_radii.top_right == 0.0 &&
        mask.corner_radii.bottom_right == 0.0 && mask.corner_radii.bottom_left == 0.0) {
        return 1.0;
    }
    return saturate(0.5 - rounded_rect_sdf(pt, mask.bounds, mask.corner_radii,
                                           mask.corner_smoothing));
}

GradientColor prepare_gradient_color(uint tag, uint color_space, Hsla solid, LinearColorStop colors[2]) {
    GradientColor output;
    if (tag == 0 || tag == 2 || tag == 3) {
        output.solid = hsla_to_rgba(solid);
    } else if (tag == 1) {
        output.color0 = hsla_to_rgba(colors[0].color);
        output.color1 = hsla_to_rgba(colors[1].color);

        // Prepare color space in vertex for avoid conversion
        // in fragment shader for performance reasons
        if (color_space == 1) {
            // Oklab
            output.color0 = srgb_to_oklab(output.color0);
            output.color1 = srgb_to_oklab(output.color1);
        }
    }

    return output;
}

float2x2 rotate2d(float angle) {
    float s = sin(angle);
    float c = cos(angle);
    return float2x2(c, -s, s, c);
}

float4 gradient_color(Background background,
                      float2 position,
                      Bounds bounds,
                      float4 solid_color, float4 color0, float4 color1) {
    float4 color;

    switch (background.tag) {
        case 0:
            color = solid_color;
            break;
        case 1: {
            // -90 degrees to match the CSS gradient angle.
            float gradient_angle = background.gradient_angle_or_pattern_height;
            float radians = (fmod(gradient_angle, 360.0) - 90.0) * (M_PI_F / 180.0);
            float2 direction = float2(cos(radians), sin(radians));

            // Expand the short side to be the same as the long side
            if (bounds.size.x > bounds.size.y) {
                direction.y *= bounds.size.y / bounds.size.x;
            } else {
                direction.x *=  bounds.size.x / bounds.size.y;
            }

            // Get the t value for the linear gradient with the color stop percentages.
            float2 half_size = bounds.size * 0.5;
            float2 center = bounds.origin + half_size;
            float2 center_to_point = position - center;
            float t = dot(center_to_point, direction) / length(direction);
            // Check the direct to determine the use x or y
            if (abs(direction.x) > abs(direction.y)) {
                t = (t + half_size.x) / bounds.size.x;
            } else {
                t = (t + half_size.y) / bounds.size.y;
            }

            // Adjust t based on the stop percentages
            t = (t - background.colors[0].percentage)
                / (background.colors[1].percentage
                - background.colors[0].percentage);
            t = clamp(t, 0.0, 1.0);

            switch (background.color_space) {
                case 0:
                    color = lerp(color0, color1, t);
                    break;
                case 1: {
                    float4 oklab_color = lerp(color0, color1, t);
                    color = oklab_to_srgb(oklab_color);
                    break;
                }
            }

            // Dither to reduce banding in gradients (especially dark/alpha).
            // Triangular-distributed noise breaks up 8-bit quantization steps.
            // ±2/255 for RGB (enough for dark-on-dark compositing),
            // ±3/255 for alpha (needs more because alpha × dark color = tiny steps).
            {
                float2 seed = position * 0.6180339887; // golden ratio spread
                float r1 = frac(sin(dot(seed, float2(12.9898, 78.233))) * 43758.5453);
                float r2 = frac(sin(dot(seed, float2(39.3460, 11.135))) * 24634.6345);
                float tri = r1 + r2 - 1.0; // triangular PDF, range [-1, +1]
                color.rgb += tri * 2.0 / 255.0;
                color.a   += tri * 3.0 / 255.0;
            }

            break;
        }
        case 2: {
            float gradient_angle_or_pattern_height = background.gradient_angle_or_pattern_height;
            float pattern_width = (gradient_angle_or_pattern_height / 65535.0f) / 255.0f;
            float pattern_interval = fmod(gradient_angle_or_pattern_height, 65535.0f) / 255.0f;
            float pattern_height = pattern_width + pattern_interval;
            float stripe_angle = M_PI_F / 4.0;
            float pattern_period = pattern_height * sin(stripe_angle);
            float2x2 rotation = rotate2d(stripe_angle);
            float2 relative_position = position - bounds.origin;
            float2 rotated_point = mul(relative_position, rotation);
            float pattern = fmod(rotated_point.x, pattern_period);
            float distance = min(pattern, pattern_period - pattern) - pattern_period * (pattern_width / pattern_height) /  2.0f;
            color = solid_color;
            color.a *= saturate(0.5 - distance);
            break;
        }
        case 3: {
            // checkerboard
            float size = background.gradient_angle_or_pattern_height;
            float2 relative_position = position - bounds.origin;

            float x_index = floor(relative_position.x / size);
            float y_index = floor(relative_position.y / size);
            float should_be_colored = (x_index + y_index) % 2.0;

            color = solid_color;
            color.a *= saturate(should_be_colored);
            break;
        }
    }

    return color;
}

// Returns the dash velocity of a corner given the dash velocity of the two
// sides, by returning the slower velocity (larger dashes).
//
// Since 0 is used for dash velocity when the border width is 0 (instead of
// +inf), this returns the other dash velocity in that case.
//
// An alternative to this might be to appropriately interpolate the dash
// velocity around the corner, but that seems overcomplicated.
float corner_dash_velocity(float dv1, float dv2) {
    if (dv1 == 0.0) {
        return dv2;
    } else if (dv2 == 0.0) {
        return dv1;
    } else {
        return min(dv1, dv2);
    }
}

// Returns alpha used to render antialiased dashes.
// `t` is within the dash when `fmod(t, period) < length`.
float dash_alpha(
    float t, float period, float length, float dash_velocity,
    float antialias_threshold
) {
    float half_period = period / 2.0;
    float half_length = length / 2.0;
    // Value in [-half_period, half_period]
    // The dash is in [-half_length, half_length]
    float centered = fmod(t + half_period - half_length, period) - half_period;
    // Signed distance for the dash, negative values are inside the dash
    float signed_distance = abs(centered) - half_length;
    // Antialiased alpha based on the signed distance
    return saturate(antialias_threshold - signed_distance / dash_velocity);
}

// This approximates distance to the nearest point to a quarter ellipse in a way
// that is sufficient for anti-aliasing when the ellipse is not very eccentric.
// The components of `point` are expected to be positive.
//
// Negative on the outside and positive on the inside.
float quarter_ellipse_sdf(float2 pt, float2 radii) {
    // Scale the space to treat the ellipse like a unit circle
    float2 circle_vec = pt / radii;
    float unit_circle_sdf = length(circle_vec) - 1.0;
    // Approximate up-scaling of the length by using the average of the radii.
    //
    // TODO: A better solution would be to use the gradient of the implicit
    // function for an ellipse to approximate a scaling factor.
    return unit_circle_sdf * (radii.x + radii.y) * -0.5;
}

/*
**
**              Quads
**
*/

struct Quad {
    uint order;
    uint border_style;
    Bounds bounds;
    ContentMask content_mask;
    Background background;
    Hsla border_color;
    Corners corner_radii;
    Edges border_widths;
    float corner_smoothing;
    uint pad;
};

struct QuadVertexOutput {
    nointerpolation uint quad_id: TEXCOORD0;
    float4 position: SV_Position;
    nointerpolation float4 border_color: COLOR0;
    nointerpolation float4 background_solid: COLOR1;
    nointerpolation float4 background_color0: COLOR2;
    nointerpolation float4 background_color1: COLOR3;
    float4 clip_distance: SV_ClipDistance;
};

struct QuadFragmentInput {
    nointerpolation uint quad_id: TEXCOORD0;
    float4 position: SV_Position;
    nointerpolation float4 border_color: COLOR0;
    nointerpolation float4 background_solid: COLOR1;
    nointerpolation float4 background_color0: COLOR2;
    nointerpolation float4 background_color1: COLOR3;
};

StructuredBuffer<Quad> quads: register(t1);

QuadVertexOutput quad_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    float2 unit_vertex = float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
    uint quad_id = batch_start_index + instance_id;
    Quad quad = quads[quad_id];
    float4 device_position = to_device_position(unit_vertex, quad.bounds);

    GradientColor gradient = prepare_gradient_color(
        quad.background.tag,
        quad.background.color_space,
        quad.background.solid,
        quad.background.colors
    );
    float4 clip_distance = distance_from_clip_rect(unit_vertex, quad.bounds, quad.content_mask.bounds);
    float4 border_color = hsla_to_rgba(quad.border_color);

    QuadVertexOutput output;
    output.position = device_position;
    output.border_color = border_color;
    output.quad_id = quad_id;
    output.background_solid = gradient.solid;
    output.background_color0 = gradient.color0;
    output.background_color1 = gradient.color1;
    output.clip_distance = clip_distance;
    return output;
}

float smooth_dash_alpha(Quad quad, SmoothSample sample, SmoothCorners prepared,
                        float2 pt, float2 center_to_point, float2 border,
                        float2 straight_border_inner_corner_to_point) {
    const float antialias_threshold = 0.5;
    const float dash_length_per_width = 2.0;
    const float dash_gap_per_width = 1.0;
    const float dash_period_per_width = dash_length_per_width + dash_gap_per_width;
    const float dv_numerator = 1.0 / dash_period_per_width;
    float2 size = quad.bounds.size;
    float4 h = prepared.horizontal_reaches;
    float4 v = prepared.vertical_reaches;

    float w_t = quad.border_widths.top;
    float w_r = quad.border_widths.right;
    float w_b = quad.border_widths.bottom;
    float w_l = quad.border_widths.left;
    float dv_t = w_t <= 0.0 ? 0.0 : dv_numerator / w_t;
    float dv_r = w_r <= 0.0 ? 0.0 : dv_numerator / w_r;
    float dv_b = w_b <= 0.0 ? 0.0 : dv_numerator / w_b;
    float dv_l = w_l <= 0.0 ? 0.0 : dv_numerator / w_l;

    float s_t = (size.x - h.x - h.y) * dv_t;
    float s_r = (size.y - v.y - v.z) * dv_r;
    float s_b = (size.x - h.z - h.w) * dv_b;
    float s_l = (size.y - v.w - v.x) * dv_l;

    float4 corner_velocities = float4(
        corner_dash_velocity(dv_t, dv_l), corner_dash_velocity(dv_t, dv_r),
        corner_dash_velocity(dv_b, dv_r), corner_dash_velocity(dv_b, dv_l));
    float4 radii = corners_to_float4(quad.corner_radii);
    float4 corner_lengths = float4(
        smooth_corner_length(smooth_corner_params(radii.x, h.x, v.x, prepared.factors)),
        smooth_corner_length(smooth_corner_params(radii.y, h.y, v.y, prepared.factors)),
        smooth_corner_length(smooth_corner_params(radii.z, h.z, v.z, prepared.factors)),
        smooth_corner_length(smooth_corner_params(radii.w, h.w, v.w, prepared.factors)));
    float4 c = corner_lengths * corner_velocities;

    float upto_tr = s_t;
    float upto_r = upto_tr + c.y;
    float upto_br = upto_r + s_r;
    float upto_b = upto_br + c.z;
    float upto_bl = upto_b + s_b;
    float upto_l = upto_bl + c.w;
    float upto_tl = upto_l + s_l;
    float max_t = upto_tl + c.x;

    float t = 0.0;
    float dash_velocity = 0.0;
    if (sample.corner != SMOOTH_NO_CORNER && sample.segment != SMOOTH_SEGMENT_STRAIGHT) {
        uint corner = sample.corner;
        float corner_length = corner_lengths[corner];
        SmoothCornerParams params =
            smooth_corner_params(radii[corner], h[corner], v[corner], prepared.factors);
        float progress = smooth_corner_progress(params, sample, corner_length);
        dash_velocity = corner_velocities[corner];
        if (corner == 0) {
            t = upto_tl + (corner_length - progress) * dash_velocity;
        } else if (corner == 1) {
            t = upto_tr + progress * dash_velocity;
        } else if (corner == 2) {
            t = upto_br + (corner_length - progress) * dash_velocity;
        } else {
            t = upto_bl + progress * dash_velocity;
        }
    } else {
        bool is_horizontal =
            straight_border_inner_corner_to_point.x < straight_border_inner_corner_to_point.y;
        if ((is_horizontal ? border.y : border.x) <= 0.0) {
            is_horizontal = !is_horizontal;
        }
        if (is_horizontal) {
            if (center_to_point.y < 0.0) {
                dash_velocity = dv_t;
                t = (pt.x - h.x) * dash_velocity;
            } else {
                dash_velocity = dv_b;
                t = upto_bl - (pt.x - h.w) * dash_velocity;
            }
        } else {
            if (center_to_point.x < 0.0) {
                dash_velocity = dv_l;
                t = upto_tl - (pt.y - v.x) * dash_velocity;
            } else {
                dash_velocity = dv_r;
                t = upto_r + (pt.y - v.y) * dash_velocity;
            }
        }
    }

    if (max_t < 1.0) {
        return 1.0;
    }
    float dash_count = floor(max_t);
    float dash_period = max_t / dash_count;
    float dash_length = dash_length_per_width / dash_period_per_width;
    return dash_alpha(t, dash_period, dash_length, dash_velocity, antialias_threshold);
}

float4 smooth_quad_color(Quad quad, float2 position, float4 background_color,
                         float4 input_border_color, float mask_alpha) {
    const float antialias_threshold = 0.5;
    float4 radii = corners_to_float4(quad.corner_radii);
    float2 size = quad.bounds.size;
    float2 pt = position - quad.bounds.origin;
    SmoothCorners prepared = smooth_prepare(size, radii, quad.corner_smoothing);
    SmoothSample sample = smooth_rect_sample(pt, size, radii, prepared);
    float outer_sdf = sample.distance;

    if (quad.border_widths.top == 0.0 && quad.border_widths.right == 0.0 &&
        quad.border_widths.bottom == 0.0 && quad.border_widths.left == 0.0) {
        return background_color *
               float4(1.0, 1.0, 1.0, saturate(antialias_threshold - outer_sdf) * mask_alpha);
    }

    float2 half_size = size / 2.0;
    float2 center_to_point = pt - half_size;
    float2 border = float2(
        center_to_point.x < 0.0 ? quad.border_widths.left : quad.border_widths.right,
        center_to_point.y < 0.0 ? quad.border_widths.top : quad.border_widths.bottom);
    if (sample.corner == 0) {
        border = float2(quad.border_widths.left, quad.border_widths.top);
    } else if (sample.corner == 1) {
        border = float2(quad.border_widths.right, quad.border_widths.top);
    } else if (sample.corner == 2) {
        border = float2(quad.border_widths.right, quad.border_widths.bottom);
    } else if (sample.corner == 3) {
        border = float2(quad.border_widths.left, quad.border_widths.bottom);
    }

    float2 reduced_border = float2(border.x == 0.0 ? -antialias_threshold : border.x,
                                   border.y == 0.0 ? -antialias_threshold : border.y);
    float2 straight_border_inner_corner_to_point =
        abs(center_to_point) - half_size + reduced_border;
    bool near_curve = sample.segment != SMOOTH_SEGMENT_STRAIGHT;

    if (straight_border_inner_corner_to_point.x < -antialias_threshold &&
        straight_border_inner_corner_to_point.y < -antialias_threshold && !near_curve) {
        return background_color * float4(1.0, 1.0, 1.0, mask_alpha);
    }

    float inner_sdf;
    if (near_curve) {
        float2 normal = abs(sample.normal);
        float2 active_sides = float2(border.x > 0.0 ? 1.0 : 0.0, border.y > 0.0 ? 1.0 : 0.0);
        float effective_width = length(border * normal) -
                                antialias_threshold * (1.0 - length(active_sides * normal));
        inner_sdf = -(outer_sdf + effective_width);
    } else {
        inner_sdf = -max(straight_border_inner_corner_to_point.x,
                         straight_border_inner_corner_to_point.y);
    }

    float4 color = background_color;
    if (max(inner_sdf, outer_sdf) < antialias_threshold) {
        float4 border_color = input_border_color;
        if (quad.border_style == 1) {
            border_color.a *= smooth_dash_alpha(quad, sample, prepared, pt, center_to_point,
                                                border, straight_border_inner_corner_to_point);
        }
        float4 blended_border = over(background_color, border_color);
        color = lerp(background_color, blended_border, saturate(antialias_threshold - inner_sdf));
    }

    return color * float4(1.0, 1.0, 1.0, saturate(antialias_threshold - outer_sdf) * mask_alpha);
}

float4 quad_fragment(QuadFragmentInput input): SV_Target {
    Quad quad = quads[input.quad_id];
    float mask_alpha = content_mask_alpha(input.position.xy, quad.content_mask);
    float4 background_color = gradient_color(quad.background, input.position.xy, quad.bounds,
    input.background_solid, input.background_color0, input.background_color1);

    bool unrounded = quad.corner_radii.top_left == 0.0 &&
        quad.corner_radii.top_right == 0.0 &&
        quad.corner_radii.bottom_left == 0.0 &&
        quad.corner_radii.bottom_right == 0.0;

    if (quad.corner_smoothing > 0.0 && !unrounded) {
        return smooth_quad_color(quad, input.position.xy, background_color, input.border_color,
                                 mask_alpha);
    }

    // Fast path when the quad is not rounded and doesn't have any border
    if (quad.border_widths.top == 0.0 &&
        quad.border_widths.left == 0.0 &&
        quad.border_widths.right == 0.0 &&
        quad.border_widths.bottom == 0.0 &&
        unrounded) {
        return background_color * float4(1.0, 1.0, 1.0, mask_alpha);
    }

    float2 size = quad.bounds.size;
    float2 half_size = size / 2.;
    float2 the_point = input.position.xy - quad.bounds.origin;
    float2 center_to_point = the_point - half_size;

    // Signed distance field threshold for inclusion of pixels. 0.5 is the
    // minimum distance between the center of the pixel and the edge.
    const float antialias_threshold = 0.5;

    // Radius of the nearest corner
    float corner_radius = pick_corner_radius(center_to_point, quad.corner_radii);

    float2 border = float2(
        center_to_point.x < 0.0 ? quad.border_widths.left : quad.border_widths.right,
        center_to_point.y < 0.0 ? quad.border_widths.top : quad.border_widths.bottom
    );

    // 0-width borders are reduced so that `inner_sdf >= antialias_threshold`.
    // The purpose of this is to not draw antialiasing pixels in this case.
    float2 reduced_border = float2(
        border.x == 0.0 ? -antialias_threshold : border.x,
        border.y == 0.0 ? -antialias_threshold : border.y
    );

    // Vector from the corner of the quad bounds to the point, after mirroring
    // the point into the bottom right quadrant. Both components are <= 0.
    float2 corner_to_point = abs(center_to_point) - half_size;

    // Vector from the point to the center of the rounded corner's circle, also
    // mirrored into bottom right quadrant.
    float2 corner_center_to_point = corner_to_point + corner_radius;

    // Whether the nearest point on the border is rounded
    bool is_near_rounded_corner =
        corner_center_to_point.x >= 0.0 &&
        corner_center_to_point.y >= 0.0;

    // Vector from straight border inner corner to point.
    //
    // 0-width borders are turned into width -1 so that inner_sdf is > 1.0 near
    // the border. Without this, antialiasing pixels would be drawn.
    float2 straight_border_inner_corner_to_point = corner_to_point + reduced_border;

    // Whether the point is beyond the inner edge of the straight border
    bool is_beyond_inner_straight_border =
        straight_border_inner_corner_to_point.x > 0.0 ||
        straight_border_inner_corner_to_point.y > 0.0;

    // Whether the point is far enough inside the quad, such that the pixels are
    // not affected by the straight border.
    bool is_within_inner_straight_border =
        straight_border_inner_corner_to_point.x < -antialias_threshold &&
        straight_border_inner_corner_to_point.y < -antialias_threshold;

    // Fast path for points that must be part of the background
    if (is_within_inner_straight_border && !is_near_rounded_corner) {
        return background_color;
    }

    // Signed distance of the point to the outside edge of the quad's border
    float outer_sdf = quad_sdf_impl(corner_center_to_point, corner_radius);

    // Approximate signed distance of the point to the inside edge of the quad's
    // border. It is negative outside this edge (within the border), and
    // positive inside.
    //
    // This is not always an accurate signed distance:
    // * The rounded portions with varying border width use an approximation of
    //   nearest-point-on-ellipse.
    // * When it is quickly known to be outside the edge, -1.0 is used.
    float inner_sdf = 0.0;
    if (corner_center_to_point.x <= 0.0 || corner_center_to_point.y <= 0.0) {
        // Fast paths for straight borders
        inner_sdf = -max(straight_border_inner_corner_to_point.x,
                        straight_border_inner_corner_to_point.y);
    } else if (is_beyond_inner_straight_border) {
        // Fast path for points that must be outside the inner edge
        inner_sdf = -1.0;
    } else if (reduced_border.x == reduced_border.y) {
        // Fast path for circular inner edge.
        inner_sdf = -(outer_sdf + reduced_border.x);
    } else {
        float2 ellipse_radii = max(float2(0.0, 0.0), float2(corner_radius, corner_radius) - reduced_border);
        inner_sdf = quarter_ellipse_sdf(corner_center_to_point, ellipse_radii);
    }

    // Negative when inside the border
    float border_sdf = max(inner_sdf, outer_sdf);

    float4 color = background_color;
    if (border_sdf < antialias_threshold) {
        float4 border_color = input.border_color;
        // Dashed border logic when border_style == 1
        if (quad.border_style == 1) {
            // Position along the perimeter in "dash space", where each dash
            // period has length 1
            float t = 0.0;

            // Total number of dash periods, so that the dash spacing can be
            // adjusted to evenly divide it
            float max_t = 0.0;

            // Border width is proportional to dash size. This is the behavior
            // used by browsers, but also avoids dashes from different segments
            // overlapping when dash size is smaller than the border width.
            //
            // Dash pattern: (2 * border width) dash, (1 * border width) gap
            const float dash_length_per_width = 2.0;
            const float dash_gap_per_width = 1.0;
            const float dash_period_per_width = dash_length_per_width + dash_gap_per_width;

            // Since the dash size is determined by border width, the density of
            // dashes varies. Multiplying a pixel distance by this returns a
            // position in dash space - it has units (dash period / pixels). So
            // a dash velocity of (1 / 10) is 1 dash every 10 pixels.
            float dash_velocity = 0.0;

            // Dividing this by the border width gives the dash velocity
            const float dv_numerator = 1.0 / dash_period_per_width;

            if (unrounded) {
                // When corners aren't rounded, the dashes are separately laid
                // out on each straight line, rather than around the whole
                // perimeter. This way each line starts and ends with a dash.
                bool is_horizontal = corner_center_to_point.x < corner_center_to_point.y;
                // Choosing the right border width for dashed borders.
                // TODO: A better solution exists taking a look at the whole file.
                // this does not fix single dashed borders at the corners
                float2 dashed_border = float2(
                    max(quad.border_widths.bottom, quad.border_widths.top),
                    max(quad.border_widths.right, quad.border_widths.left)
                );
                float border_width = is_horizontal ? dashed_border.x : dashed_border.y;
                dash_velocity = dv_numerator / border_width;
                t = is_horizontal ? the_point.x : the_point.y;
                t *= dash_velocity;
                max_t = is_horizontal ? size.x : size.y;
                max_t *= dash_velocity;
            } else {
                // When corners are rounded, the dashes are laid out clockwise
                // around the whole perimeter.

                float r_tr = quad.corner_radii.top_right;
                float r_br = quad.corner_radii.bottom_right;
                float r_bl = quad.corner_radii.bottom_left;
                float r_tl = quad.corner_radii.top_left;

                float w_t = quad.border_widths.top;
                float w_r = quad.border_widths.right;
                float w_b = quad.border_widths.bottom;
                float w_l = quad.border_widths.left;

                // Straight side dash velocities
                float dv_t = w_t <= 0.0 ? 0.0 : dv_numerator / w_t;
                float dv_r = w_r <= 0.0 ? 0.0 : dv_numerator / w_r;
                float dv_b = w_b <= 0.0 ? 0.0 : dv_numerator / w_b;
                float dv_l = w_l <= 0.0 ? 0.0 : dv_numerator / w_l;

                // Straight side lengths in dash space
                float s_t = (size.x - r_tl - r_tr) * dv_t;
                float s_r = (size.y - r_tr - r_br) * dv_r;
                float s_b = (size.x - r_br - r_bl) * dv_b;
                float s_l = (size.y - r_bl - r_tl) * dv_l;

                float corner_dash_velocity_tr = corner_dash_velocity(dv_t, dv_r);
                float corner_dash_velocity_br = corner_dash_velocity(dv_b, dv_r);
                float corner_dash_velocity_bl = corner_dash_velocity(dv_b, dv_l);
                float corner_dash_velocity_tl = corner_dash_velocity(dv_t, dv_l);

                // Corner lengths in dash space
                float c_tr = r_tr * (M_PI_F / 2.0) * corner_dash_velocity_tr;
                float c_br = r_br * (M_PI_F / 2.0) * corner_dash_velocity_br;
                float c_bl = r_bl * (M_PI_F / 2.0) * corner_dash_velocity_bl;
                float c_tl = r_tl * (M_PI_F / 2.0) * corner_dash_velocity_tl;

                // Cumulative dash space upto each segment
                float upto_tr = s_t;
                float upto_r = upto_tr + c_tr;
                float upto_br = upto_r + s_r;
                float upto_b = upto_br + c_br;
                float upto_bl = upto_b + s_b;
                float upto_l = upto_bl + c_bl;
                float upto_tl = upto_l + s_l;
                max_t = upto_tl + c_tl;

                if (is_near_rounded_corner) {
                    float radians = atan2(corner_center_to_point.y, corner_center_to_point.x);
                    float corner_t = radians * corner_radius;

                    if (center_to_point.x >= 0.0) {
                        if (center_to_point.y < 0.0) {
                            dash_velocity = corner_dash_velocity_tr;
                            // Subtracted because radians is pi/2 to 0 when
                            // going clockwise around the top right corner,
                            // since the y axis has been flipped
                            t = upto_r - corner_t * dash_velocity;
                        } else {
                            dash_velocity = corner_dash_velocity_br;
                            // Added because radians is 0 to pi/2 when going
                            // clockwise around the bottom-right corner
                            t = upto_br + corner_t * dash_velocity;
                        }
                    } else {
                        if (center_to_point.y >= 0.0) {
                            dash_velocity = corner_dash_velocity_bl;
                            // Subtracted because radians is pi/1 to 0 when
                            // going clockwise around the bottom-left corner,
                            // since the x axis has been flipped
                            t = upto_l - corner_t * dash_velocity;
                        } else {
                            dash_velocity = corner_dash_velocity_tl;
                            // Added because radians is 0 to pi/2 when going
                            // clockwise around the top-left corner, since both
                            // axis were flipped
                            t = upto_tl + corner_t * dash_velocity;
                        }
                    }
                } else {
                    // Straight borders
                    bool is_horizontal = corner_center_to_point.x < corner_center_to_point.y;
                    if (is_horizontal) {
                        if (center_to_point.y < 0.0) {
                            dash_velocity = dv_t;
                            t = (the_point.x - r_tl) * dash_velocity;
                        } else {
                            dash_velocity = dv_b;
                            t = upto_bl - (the_point.x - r_bl) * dash_velocity;
                        }
                    } else {
                        if (center_to_point.x < 0.0) {
                            dash_velocity = dv_l;
                            t = upto_tl - (the_point.y - r_tl) * dash_velocity;
                        } else {
                            dash_velocity = dv_r;
                            t = upto_r + (the_point.y - r_tr) * dash_velocity;
                        }
                    }
                }
            }
            float dash_length = dash_length_per_width / dash_period_per_width;
            float desired_dash_gap = dash_gap_per_width / dash_period_per_width;

            // Straight borders should start and end with a dash, so max_t is
            // reduced to cause this.
            max_t -= unrounded ? dash_length : 0.0;
            if (max_t >= 1.0) {
                // Adjust dash gap to evenly divide max_t
                float dash_count = floor(max_t);
                float dash_period = max_t / dash_count;
                border_color.a *= dash_alpha(t, dash_period, dash_length, dash_velocity, antialias_threshold);
            } else if (unrounded) {
                // When there isn't enough space for the full gap between the
                // two start / end dashes of a straight border, reduce gap to
                // make them fit.
                float dash_gap = max_t - dash_length;
                if (dash_gap > 0.0) {
                    float dash_period = dash_length + dash_gap;
                    border_color.a *= dash_alpha(t, dash_period, dash_length, dash_velocity, antialias_threshold);
                }
            }
        }

        // Blend the border on top of the background and then linearly interpolate
        // between the two as we slide inside the background.
        float4 blended_border = over(background_color, border_color);
        color = lerp(background_color, blended_border,
                    saturate(antialias_threshold - inner_sdf));
    }

    return color * float4(1.0, 1.0, 1.0,
        saturate(antialias_threshold - outer_sdf) * mask_alpha);
}

/*
**
**              Shadows
**
*/

struct Shadow {
    uint order;
    float blur_radius;
    Bounds bounds;
    Corners corner_radii;
    ContentMask content_mask;
    Hsla color;
    Bounds element_bounds;
    Corners element_corner_radii;
    uint inset;
    float corner_smoothing;
};

struct ShadowVertexOutput {
    nointerpolation uint shadow_id: TEXCOORD0;
    float4 position: SV_Position;
    nointerpolation float4 color: COLOR;
    float4 clip_distance: SV_ClipDistance;
};

struct ShadowFragmentInput {
  nointerpolation uint shadow_id: TEXCOORD0;
  float4 position: SV_Position;
  nointerpolation float4 color: COLOR;
};

StructuredBuffer<Shadow> shadows: register(t1);

ShadowVertexOutput shadow_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    float2 unit_vertex = float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
    uint shadow_id = batch_start_index + instance_id;
    Shadow shadow = shadows[shadow_id];

    Bounds bounds;
    if (shadow.inset != 0u) {
        bounds = shadow.element_bounds;
    } else {
        // Leave room for the gaussian tail outside the shadow rect.
        float margin = 3.0 * shadow.blur_radius;
        bounds = shadow.bounds;
        bounds.origin -= margin;
        bounds.size += 2.0 * margin;
    }

    float4 device_position = to_device_position(unit_vertex, bounds);
    float4 clip_distance = distance_from_clip_rect(unit_vertex, bounds, shadow.content_mask.bounds);
    float4 color = hsla_to_rgba(shadow.color);

    ShadowVertexOutput output;
    output.position = device_position;
    output.color = color;
    output.shadow_id = shadow_id;
    output.clip_distance = clip_distance;

    return output;
}

float smooth_shadow_alpha(Shadow shadow, float2 position) {
    float alpha = 0.0;
    if (shadow.blur_radius == 0.0) {
        alpha = saturate(0.5 - smooth_rect_sdf(position, shadow.bounds, shadow.corner_radii,
                                               shadow.corner_smoothing));
    } else {
        float blur_limit = 3.0 * shadow.blur_radius;
        float2 origin = shadow.bounds.origin;
        float2 size = shadow.bounds.size;
        float2 half_size = size / 2.0;
        float2 center_to_point = position - (origin + half_size);
        float2 outside_delta = max(abs(center_to_point) - half_size, (float2)0.0);
        if (dot(outside_delta, outside_delta) <= blur_limit * blur_limit) {
            float4 radii = corners_to_float4(shadow.corner_radii);
            SmoothCorners prepared = smooth_prepare(size, radii, shadow.corner_smoothing);
            float2 pt = position - origin;
            float edge_depth = min(min(pt.x, size.x - pt.x), min(pt.y, size.y - pt.y));
            if (!smooth_has_corner_candidate(pt, size, prepared) && edge_depth >= blur_limit) {
                alpha = 1.0;
            } else {
                float distance = smooth_rect_sample(pt, size, radii, prepared).distance;
                float normalized = distance / (sqrt(2.0) * shadow.blur_radius);
                alpha = saturate(0.5 - 0.5 * erf(float2(normalized, normalized)).x);
            }
        }
    }

    if (shadow.inset != 0u) {
        alpha = (1.0 - alpha) *
                saturate(0.5 - rounded_rect_sdf(position, shadow.element_bounds,
                                                shadow.element_corner_radii,
                                                shadow.corner_smoothing));
    }
    return alpha;
}

float4 shadow_fragment(ShadowFragmentInput input): SV_TARGET {
    Shadow shadow = shadows[input.shadow_id];
    if (shadow.corner_smoothing > 0.0) {
        return input.color * float4(1., 1., 1.,
            smooth_shadow_alpha(shadow, input.position.xy) *
            content_mask_alpha(input.position.xy, shadow.content_mask));
    }

    float2 half_size = shadow.bounds.size / 2.;
    float2 center = shadow.bounds.origin + half_size;
    float2 point0 = input.position.xy - center;
    float corner_radius = pick_corner_radius(point0, shadow.corner_radii);

    float alpha;
    if (shadow.blur_radius == 0.) {
        float distance = quad_sdf(input.position.xy, shadow.bounds, shadow.corner_radii);
        alpha = saturate(0.5 - distance);
    } else {
        // The signal is only non-zero in a limited range, so don't waste samples
        float low = point0.y - half_size.y;
        float high = point0.y + half_size.y;
        float start = clamp(-3. * shadow.blur_radius, low, high);
        float end = clamp(3. * shadow.blur_radius, low, high);

        // Accumulate samples (we can get away with surprisingly few samples)
        float step = (end - start) / 4.;
        float y = start + step * 0.5;
        alpha = 0.;
        for (int i = 0; i < 4; i++) {
            alpha += blur_along_x(point0.x, point0.y - y, shadow.blur_radius,
                                corner_radius, half_size) *
                    gaussian(y, shadow.blur_radius) * step;
            y += step;
        }
    }

    if (shadow.inset != 0u) {
        // The inset shadow is the complement of the (blurred) hole rect, clipped to the element.
        // `saturate(0.5 - d)` gives a 1-pixel antialiased edge: d <= -0.5 -> 1, d >= 0.5 -> 0.
        alpha = 1.0 - alpha;
        float element_distance = quad_sdf(input.position.xy, shadow.element_bounds,
                                          shadow.element_corner_radii);
        alpha *= saturate(0.5 - element_distance);
    }

    return input.color * float4(1., 1., 1.,
        alpha * content_mask_alpha(input.position.xy, shadow.content_mask));
}

/*
**
**              Path Rasterization
**
*/

struct PathRasterizationSprite {
    float2 xy_position;
    float2 st_position;
    Background color;
    Bounds bounds;
    ContentMask content_mask;
};

StructuredBuffer<PathRasterizationSprite> path_rasterization_sprites: register(t1);

struct PathVertexOutput {
    float4 position: SV_Position;
    float2 st_position: TEXCOORD0;
    nointerpolation uint vertex_id: TEXCOORD1;
    float4 clip_distance: SV_ClipDistance;
};

struct PathFragmentInput {
    float4 position: SV_Position;
    float2 st_position: TEXCOORD0;
    nointerpolation uint vertex_id: TEXCOORD1;
};

PathVertexOutput path_rasterization_vertex(uint vertex_id: SV_VertexID) {
    PathRasterizationSprite sprite = path_rasterization_sprites[vertex_id];

    PathVertexOutput output;
    output.position = to_device_position_impl(sprite.xy_position);
    output.st_position = sprite.st_position;
    output.vertex_id = vertex_id;
    output.clip_distance = distance_from_clip_rect_impl(sprite.xy_position, sprite.bounds);

    return output;
}

float4 path_rasterization_fragment(PathFragmentInput input): SV_Target {
    float2 dx = ddx(input.st_position);
    float2 dy = ddy(input.st_position);
    PathRasterizationSprite sprite = path_rasterization_sprites[input.vertex_id];

    Background background = sprite.color;
    Bounds bounds = sprite.bounds;

    float alpha;
    if (length(float2(dx.x, dy.x))) {
        alpha = 1.0;
    } else {
        float2 gradient = 2.0 * input.st_position.xx * float2(dx.x, dy.x) - float2(dx.y, dy.y);
        float f = input.st_position.x * input.st_position.x - input.st_position.y;
        float distance = f / length(gradient);
        alpha = saturate(0.5 - distance);
    }

    GradientColor gradient = prepare_gradient_color(
        background.tag, background.color_space, background.solid, background.colors);

    float4 color = gradient_color(background, input.position.xy, bounds,
        gradient.solid, gradient.color0, gradient.color1);
    alpha *= content_mask_alpha(input.position.xy, sprite.content_mask);
    return float4(color.rgb * color.a * alpha, alpha * color.a);
}

/*
**
**              Path Sprites
**
*/

struct PathSprite {
    Bounds bounds;
};

struct PathSpriteVertexOutput {
    float4 position: SV_Position;
    float2 texture_coords: TEXCOORD0;
};

StructuredBuffer<PathSprite> path_sprites: register(t1);

PathSpriteVertexOutput path_sprite_vertex(uint vertex_id: SV_VertexID, uint sprite_id: SV_InstanceID) {
    float2 unit_vertex = float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
    PathSprite sprite = path_sprites[sprite_id];

    // Don't apply content mask because it was already accounted for when rasterizing the path
    float4 device_position = to_device_position(unit_vertex, sprite.bounds);

    float2 screen_position = sprite.bounds.origin + unit_vertex * sprite.bounds.size;
    float2 texture_coords = screen_position / global_viewport_size;

    PathSpriteVertexOutput output;
    output.position = device_position;
    output.texture_coords = texture_coords;
    return output;
}

float4 path_sprite_fragment(PathSpriteVertexOutput input): SV_Target {
    return t_sprite.Sample(s_sprite, input.texture_coords);
}

/*
**
**              Underlines
**
*/

struct Underline {
    uint order;
    uint pad;
    Bounds bounds;
    ContentMask content_mask;
    Hsla color;
    float thickness;
    uint wavy;
};

struct UnderlineVertexOutput {
  nointerpolation uint underline_id: TEXCOORD0;
  float4 position: SV_Position;
  nointerpolation float4 color: COLOR;
  float4 clip_distance: SV_ClipDistance;
};

struct UnderlineFragmentInput {
  nointerpolation uint underline_id: TEXCOORD0;
  float4 position: SV_Position;
  nointerpolation float4 color: COLOR;
};

StructuredBuffer<Underline> underlines: register(t1);

UnderlineVertexOutput underline_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    float2 unit_vertex = float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
    uint underline_id = batch_start_index + instance_id;
    Underline underline = underlines[underline_id];
    float4 device_position = to_device_position(unit_vertex, underline.bounds);
    float4 clip_distance = distance_from_clip_rect(unit_vertex, underline.bounds,
                                                    underline.content_mask.bounds);
    float4 color = hsla_to_rgba(underline.color);

    UnderlineVertexOutput output;
    output.position = device_position;
    output.color = color;
    output.underline_id = underline_id;
    output.clip_distance = clip_distance;
    return output;
}

float4 underline_fragment(UnderlineFragmentInput input): SV_Target {
    const float WAVE_FREQUENCY = 2.0;
    const float WAVE_HEIGHT_RATIO = 0.8;

    Underline underline = underlines[input.underline_id];
    float mask_alpha = content_mask_alpha(input.position.xy, underline.content_mask);
    if (underline.wavy) {
        float half_thickness = underline.thickness * 0.5;
        float2 origin = underline.bounds.origin;

        float2 st = ((input.position.xy - origin) / underline.bounds.size.y) - float2(0., 0.5);
        float frequency = (M_PI_F * WAVE_FREQUENCY * underline.thickness) / underline.bounds.size.y;
        float amplitude = (underline.thickness * WAVE_HEIGHT_RATIO) / underline.bounds.size.y;

        float sine = sin(st.x * frequency) * amplitude;
        float dSine = cos(st.x * frequency) * amplitude * frequency;
        float distance = (st.y - sine) / sqrt(1. + dSine * dSine);
        float distance_in_pixels = distance * underline.bounds.size.y;
        float distance_from_top_border = distance_in_pixels - half_thickness;
        float distance_from_bottom_border = distance_in_pixels + half_thickness;
        float alpha = saturate(
            0.5 - max(-distance_from_bottom_border, distance_from_top_border));
        return input.color * float4(1., 1., 1., alpha * mask_alpha);
    } else {
        return input.color * float4(1., 1., 1., mask_alpha);
    }
}

/*
**
**              Monochrome sprites
**
*/

struct MonochromeSprite {
    uint order;
    uint pad;
    AtlasTile tile;
    Bounds bounds;
    ContentMask content_mask;
    Hsla color;
    TransformationMatrix transformation;
};

struct MonochromeSpriteVertexOutput {
    float4 position: SV_Position;
    float2 tile_position: POSITION;
    nointerpolation float4 color: COLOR;
    float4 clip_distance: SV_ClipDistance;
    nointerpolation uint sprite_id: TEXCOORD1;
};

struct MonochromeSpriteFragmentInput {
    float4 position: SV_Position;
    float2 tile_position: POSITION;
    nointerpolation float4 color: COLOR;
    float4 clip_distance: SV_ClipDistance;
    nointerpolation uint sprite_id: TEXCOORD1;
};

StructuredBuffer<MonochromeSprite> mono_sprites: register(t1);

MonochromeSpriteVertexOutput monochrome_sprite_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    float2 unit_vertex = float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
    uint sprite_id = batch_start_index + instance_id;
    MonochromeSprite sprite = mono_sprites[sprite_id];
    float4 device_position =
        to_device_position_transformed(unit_vertex, sprite.bounds, sprite.transformation);
    float4 clip_distance = distance_from_clip_rect_transformed(unit_vertex, sprite.bounds, sprite.content_mask.bounds, sprite.transformation);
    float2 tile_position = to_tile_position(unit_vertex, sprite.tile);
    float4 color = hsla_to_rgba(sprite.color);

    MonochromeSpriteVertexOutput output;
    output.position = device_position;
    output.tile_position = tile_position;
    output.color = color;
    output.clip_distance = clip_distance;
    output.sprite_id = sprite_id;
    return output;
}

float4 monochrome_sprite_fragment(MonochromeSpriteFragmentInput input): SV_Target {
    float sample = t_sprite.Sample(s_sprite, input.tile_position).r;
    float alpha_corrected = apply_contrast_and_gamma_correction(sample, input.color.rgb, grayscale_enhanced_contrast, gamma_ratios);
    float mask_alpha = content_mask_alpha(input.position.xy, mono_sprites[input.sprite_id].content_mask);
    return float4(input.color.rgb, input.color.a * alpha_corrected * mask_alpha);
}

MonochromeSpriteVertexOutput subpixel_sprite_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    return monochrome_sprite_vertex(vertex_id, instance_id);
}

SubpixelSpriteFragmentOutput subpixel_sprite_fragment(MonochromeSpriteFragmentInput input) {
    float3 sample = t_sprite.Sample(s_sprite, input.tile_position).rgb;
    if (is_bgr) {
        sample = sample.bgr;
    }
    float3 alpha_corrected = apply_contrast_and_gamma_correction3(sample, input.color.rgb, subpixel_enhanced_contrast, gamma_ratios);

    float mask_alpha = content_mask_alpha(input.position.xy, mono_sprites[input.sprite_id].content_mask);
    SubpixelSpriteFragmentOutput output;
    output.foreground = float4(input.color.rgb, 1.0f);
    output.alpha = float4(input.color.a * alpha_corrected * mask_alpha, 1.0f);
    return output;
}

/*
**
**              Polychrome sprites
**
*/

struct PolychromeSprite {
    uint order;
    float corner_smoothing;
    AtlasTile tile;
    uint grayscale;
    float opacity;
    Bounds bounds;
    ContentMask content_mask;
    Corners corner_radii;
};

struct PolychromeSpriteVertexOutput {
    nointerpolation uint sprite_id: TEXCOORD0;
    float4 position: SV_Position;
    float2 tile_position: POSITION;
    float4 clip_distance: SV_ClipDistance;
};

struct PolychromeSpriteFragmentInput {
    nointerpolation uint sprite_id: TEXCOORD0;
    float4 position: SV_Position;
    float2 tile_position: POSITION;
};

StructuredBuffer<PolychromeSprite> poly_sprites: register(t1);

PolychromeSpriteVertexOutput polychrome_sprite_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    float2 unit_vertex = float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
    uint sprite_id = batch_start_index + instance_id;
    PolychromeSprite sprite = poly_sprites[sprite_id];
    float4 device_position = to_device_position(unit_vertex, sprite.bounds);
    float4 clip_distance = distance_from_clip_rect(unit_vertex, sprite.bounds,
                                                    sprite.content_mask.bounds);
    float2 tile_position = to_tile_position(unit_vertex, sprite.tile);

    PolychromeSpriteVertexOutput output;
    output.position = device_position;
    output.tile_position = tile_position;
    output.sprite_id = sprite_id;
    output.clip_distance = clip_distance;
    return output;
}

float4 polychrome_sprite_fragment(PolychromeSpriteFragmentInput input): SV_Target {
    PolychromeSprite sprite = poly_sprites[input.sprite_id];
    float4 sample = t_sprite.Sample(s_sprite, input.tile_position);
    float distance = rounded_rect_sdf(input.position.xy, sprite.bounds, sprite.corner_radii,
                                      sprite.corner_smoothing);

    float4 color = sample;
    if (sprite.grayscale != 0u) {
        float3 grayscale = dot(color.rgb, GRAYSCALE_FACTORS);
        color = float4(grayscale, sample.a);
    }
    color.a *= sprite.opacity * saturate(0.5 - distance) *
        content_mask_alpha(input.position.xy, sprite.content_mask);
    return color;
}

/*
**
**              Wide-gamut present
**
*/

// The scene is rendered with sRGB-encoded values into a half-float offscreen
// target, so blending matches the other backends. This final pass converts for
// the swap chain according to color_output_mode:
//
// - scRGB (advanced color): apply the extended sRGB EOTF, sign-preserving, so
//   components outside [0, 1] reach the compositor instead of being clipped;
//   the OS maps them to the panel.
// - display-referred (SDR): apply the EOTF, map linear sRGB into the monitor
//   gamut with the ICC-derived matrix, clip, and re-encode for 8 bits. This is
//   Chromium's SDR behavior: the profile's primaries define the gamut and an
//   sRGB transfer is assumed.

struct WidePresentVertexOutput {
    float4 position: SV_Position;
};

WidePresentVertexOutput wide_present_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    float2 unit_vertex = float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
    WidePresentVertexOutput output;
    output.position = float4(unit_vertex * float2(2., -2.) + float2(-1., 1.), 0., 1.);
    return output;
}

float srgb_eotf_extended(float encoded) {
    float magnitude = abs(encoded);
    float decoded = magnitude <= 0.04045
        ? magnitude / 12.92
        : pow((magnitude + 0.055) / 1.055, 2.4);
    return encoded < 0. ? -decoded : decoded;
}

float srgb_oetf(float linear_value) {
    return linear_value <= 0.0031308
        ? linear_value * 12.92
        : 1.055 * pow(linear_value, 1. / 2.4) - 0.055;
}

float4 wide_present_fragment(WidePresentVertexOutput input): SV_Target {
    float4 color = t_sprite.Load(int3(input.position.xy, 0));
    float3 scene_linear = float3(
        srgb_eotf_extended(color.r),
        srgb_eotf_extended(color.g),
        srgb_eotf_extended(color.b));
    // The scene's alpha blending accumulates with DestBlendAlpha = ONE. An
    // 8-bit UNORM target used to clamp that to 1 on every write; the float
    // target doesn't, so clamp here before the compositor sees it. Alphas are
    // non-negative, making one final clamp equivalent to per-write clamping.
    float alpha = saturate(color.a);

    if (color_output_mode == 1u) {
        float3 display_linear = saturate(float3(
            dot(color_matrix_r0.xyz, scene_linear),
            dot(color_matrix_r1.xyz, scene_linear),
            dot(color_matrix_r2.xyz, scene_linear)));
        return float4(
            srgb_oetf(display_linear.r),
            srgb_oetf(display_linear.g),
            srgb_oetf(display_linear.b),
            alpha);
    }
    return float4(scene_linear, alpha);
}
