const M_PI_F: f32 = 3.1415926;

struct Bounds {
    origin: vec2<f32>,
    size: vec2<f32>,
}

struct Corners {
    top_left: f32,
    top_right: f32,
    bottom_right: f32,
    bottom_left: f32,
}

struct ContentMask {
    bounds: Bounds,
    corner_radii: Corners,
    corner_smoothing: f32,
    pad: f32,
}

// Selects corner radius based on quadrant.
fn pick_corner_radius(center_to_point: vec2<f32>, radii: Corners) -> f32 {
    if (center_to_point.x < 0.0) {
        if (center_to_point.y < 0.0) {
            return radii.top_left;
        } else {
            return radii.bottom_left;
        }
    } else {
        if (center_to_point.y < 0.0) {
            return radii.top_right;
        } else {
            return radii.bottom_right;
        }
    }
}

// Signed distance of the point to the quad's border - positive outside the
// border, and negative inside.
//
// See comments on similar code using `quad_sdf_impl` in `fs_quad` for
// explanation.
fn content_mask_alpha(point: vec2<f32>, mask: ContentMask) -> f32 {
    if (mask.corner_radii.top_left == 0.0 && mask.corner_radii.top_right == 0.0 &&
        mask.corner_radii.bottom_right == 0.0 && mask.corner_radii.bottom_left == 0.0) {
        return 1.0;
    }
    return saturate(0.5 - rounded_rect_sdf(point, mask.bounds, mask.corner_radii,
                                           mask.corner_smoothing));
}

fn rounded_rect_sdf(point: vec2<f32>, bounds: Bounds, corner_radii: Corners,
                    corner_smoothing: f32) -> f32 {
    if (corner_smoothing <= 0.0) {
        return quad_sdf(point, bounds, corner_radii);
    }
    return smooth_rect_sdf(point, bounds, corner_radii, corner_smoothing);
}

fn quad_sdf(point: vec2<f32>, bounds: Bounds, corner_radii: Corners) -> f32 {
    let half_size = bounds.size / 2.0;
    let center = bounds.origin + half_size;
    let center_to_point = point - center;
    let corner_radius = pick_corner_radius(center_to_point, corner_radii);
    let corner_to_point = abs(center_to_point) - half_size;
    let corner_center_to_point = corner_to_point + corner_radius;
    return quad_sdf_impl(corner_center_to_point, corner_radius);
}

fn quad_sdf_impl(corner_center_to_point: vec2<f32>, corner_radius: f32) -> f32 {
    if (corner_radius == 0.0) {
        // Fast path for unrounded corners.
        return max(corner_center_to_point.x, corner_center_to_point.y);
    } else {
        // Signed distance of the point from a quad that is inset by corner_radius.
        // It is negative inside this quad, and positive outside.
        let signed_distance_to_inset_quad =
            // 0 inside the inset quad, and positive outside.
            length(max(vec2<f32>(0.0), corner_center_to_point)) +
            // 0 outside the inset quad, and negative inside.
            min(0.0, max(corner_center_to_point.x, corner_center_to_point.y));

        return signed_distance_to_inset_quad - corner_radius;
    }
}

// --- corner smoothing --- //
// Derived from figma-squircle by Tien Pham (MIT), see THIRD_PARTY_NOTICES.md.

const SMOOTH_EPSILON: f32 = 0.000001;
const SMOOTH_SEGMENT_STRAIGHT: u32 = 0u;
const SMOOTH_SEGMENT_FIRST_CUBIC: u32 = 1u;
const SMOOTH_SEGMENT_ARC: u32 = 2u;
const SMOOTH_SEGMENT_SECOND_CUBIC: u32 = 3u;
const SMOOTH_NO_CORNER: u32 = 4u;

struct SmoothCorners {
    horizontal_reaches: vec4<f32>,
    vertical_reaches: vec4<f32>,
    factors: vec4<f32>,
}

struct SmoothAxis {
    a: f32,
    b: f32,
    p: f32,
}

struct SmoothCornerParams {
    radius: f32,
    smoothing: f32,
    arc_sweep: f32,
    c: f32,
    d: f32,
    horizontal: SmoothAxis,
    vertical: SmoothAxis,
}

struct SmoothCubicPoint {
    point: vec2<f32>,
    tangent: vec2<f32>,
    distance: f32,
    path_t: f32,
}

struct SmoothSample {
    distance: f32,
    normal: vec2<f32>,
    path_t: f32,
    segment: u32,
    corner: u32,
}

fn corners_to_vec4(corners: Corners) -> vec4<f32> {
    return vec4<f32>(corners.top_left, corners.top_right, corners.bottom_right,
                     corners.bottom_left);
}

fn smooth_split_side(side_length: f32, first_radius: f32, second_radius: f32) -> vec2<f32> {
    let total_radius = first_radius + second_radius;
    if (total_radius == 0.0) {
        return vec2<f32>(0.0);
    }
    let first_budget = side_length * first_radius / total_radius;
    return vec2<f32>(first_budget, side_length - first_budget);
}

fn smooth_factors(corner_smoothing: f32) -> vec4<f32> {
    let smoothing = clamp(corner_smoothing, 0.0, 1.0);
    let arc_sweep = 0.5 * M_PI_F * (1.0 - smoothing);
    let beta = (M_PI_F / 4.0) * smoothing;
    let join_handle_factor = tan(0.5 * beta);
    return vec4<f32>(
        smoothing,
        sin(0.5 * arc_sweep) * sqrt(2.0),
        join_handle_factor * cos(beta),
        join_handle_factor * sin(beta),
    );
}

fn smooth_prepare(size: vec2<f32>, radii_values: vec4<f32>, corner_smoothing: f32) -> SmoothCorners {
    var radii = array<f32, 4>(
        max(radii_values.x, 0.0),
        max(radii_values.y, 0.0),
        max(radii_values.z, 0.0),
        max(radii_values.w, 0.0),
    );
    var budgets = array<f32, 4>(-1.0, -1.0, -1.0, -1.0);
    var order = array<u32, 4>(0u, 1u, 3u, 2u);
    var horizontal_neighbors = array<u32, 4>(1u, 0u, 3u, 2u);
    var vertical_neighbors = array<u32, 4>(3u, 2u, 1u, 0u);

    for (var sweep = 0u; sweep < 3u; sweep += 1u) {
        for (var index = 0u; index < 3u - sweep; index += 1u) {
            if (radii[order[index]] < radii[order[index + 1u]]) {
                let swap = order[index];
                order[index] = order[index + 1u];
                order[index + 1u] = swap;
            }
        }
    }

    for (var rank = 0u; rank < 4u; rank += 1u) {
        let corner = order[rank];
        let radius = radii[corner];
        let horizontal_neighbor = horizontal_neighbors[corner];
        let vertical_neighbor = vertical_neighbors[corner];

        let horizontal_radius = radii[horizontal_neighbor];
        var horizontal_budget = 0.0;
        if (radius != 0.0 || horizontal_radius != 0.0) {
            if (budgets[horizontal_neighbor] >= 0.0) {
                horizontal_budget = size.x - budgets[horizontal_neighbor];
            } else {
                horizontal_budget = size.x * radius / (radius + horizontal_radius);
            }
        }

        let vertical_radius = radii[vertical_neighbor];
        var vertical_budget = 0.0;
        if (radius != 0.0 || vertical_radius != 0.0) {
            if (budgets[vertical_neighbor] >= 0.0) {
                vertical_budget = size.y - budgets[vertical_neighbor];
            } else {
                vertical_budget = size.y * radius / (radius + vertical_radius);
            }
        }

        let budget = max(0.0, min(horizontal_budget, vertical_budget));
        budgets[corner] = budget;
        radii[corner] = min(radius, budget);
    }

    let top = smooth_split_side(size.x, radii[0], radii[1]);
    let bottom = smooth_split_side(size.x, radii[3], radii[2]);
    let left = smooth_split_side(size.y, radii[0], radii[3]);
    let right = smooth_split_side(size.y, radii[1], radii[2]);
    let horizontal_budgets = max(vec4<f32>(top.x, top.y, bottom.y, bottom.x), vec4<f32>(0.0));
    let vertical_budgets = max(vec4<f32>(left.x, right.x, right.y, left.y), vec4<f32>(0.0));
    let clamped_radii = min(max(radii_values, vec4<f32>(0.0)),
                            min(horizontal_budgets, vertical_budgets));
    let reaches = clamped_radii * (1.0 + clamp(corner_smoothing, 0.0, 1.0));

    var prepared: SmoothCorners;
    prepared.horizontal_reaches = min(reaches, horizontal_budgets);
    prepared.vertical_reaches = min(reaches, vertical_budgets);
    prepared.factors = smooth_factors(corner_smoothing);
    return prepared;
}

fn smooth_corner_params(corner_radius: f32, horizontal_reach: f32, vertical_reach: f32,
                        factors: vec4<f32>) -> SmoothCornerParams {
    let safe_horizontal_reach = max(horizontal_reach, 0.0);
    let safe_vertical_reach = max(vertical_reach, 0.0);
    let radius = min(max(corner_radius, 0.0), min(safe_horizontal_reach, safe_vertical_reach));
    let smoothing = select(0.0, factors.x, radius > SMOOTH_EPSILON);
    let desired_reach = radius * (1.0 + smoothing);
    let arc_delta = factors.y * radius;
    let c = factors.z * radius;
    let d = factors.w * radius;
    let core_length = arc_delta + c + d;
    let ideal_b = max((desired_reach - core_length) / 3.0, 0.0);
    let horizontal_available = max(safe_horizontal_reach - core_length, 0.0);
    let vertical_available = max(safe_vertical_reach - core_length, 0.0);
    let shared_b = min(ideal_b, min(horizontal_available * (5.0 / 6.0),
                                    vertical_available * (5.0 / 6.0)));

    var params: SmoothCornerParams;
    params.radius = radius;
    params.smoothing = smoothing;
    params.arc_sweep = 0.5 * M_PI_F * (1.0 - smoothing);
    params.c = c;
    params.d = d;
    params.horizontal = SmoothAxis(horizontal_available - shared_b, shared_b, safe_horizontal_reach);
    params.vertical = SmoothAxis(vertical_available - shared_b, shared_b, safe_vertical_reach);
    return params;
}

fn smooth_cubic_point(axis: SmoothAxis, c: f32, d: f32, t: f32) -> vec2<f32> {
    let x1 = 3.0 * axis.a;
    let x2 = 3.0 * (axis.b - axis.a);
    let x3 = axis.a - 2.0 * axis.b + c;
    return vec2<f32>(t * (x1 + t * (x2 + t * x3)), d * t * t * t);
}

fn smooth_cubic_derivative(axis: SmoothAxis, c: f32, d: f32, t: f32) -> vec2<f32> {
    let x1 = 3.0 * axis.a;
    let x2 = 3.0 * (axis.b - axis.a);
    let x3 = axis.a - 2.0 * axis.b + c;
    return vec2<f32>(x1 + 2.0 * x2 * t + 3.0 * x3 * t * t, 3.0 * d * t * t);
}

fn smooth_cubic_second_derivative(axis: SmoothAxis, c: f32, d: f32, t: f32) -> vec2<f32> {
    let x2 = 3.0 * (axis.b - axis.a);
    let x3 = axis.a - 2.0 * axis.b + c;
    return vec2<f32>(2.0 * x2 + 6.0 * x3 * t, 6.0 * d * t);
}

fn smooth_closest_cubic(point: vec2<f32>, axis: SmoothAxis, c: f32, d: f32) -> SmoothCubicPoint {
    let y_seed = pow(clamp(point.y / max(d, 0.00001), 0.0, 1.0), 1.0 / 3.0);
    let chord = smooth_cubic_point(axis, c, d, 1.0);
    let chord_seed = clamp(dot(point, chord) / max(dot(chord, chord), SMOOTH_EPSILON), 0.0, 1.0);
    let y_delta = smooth_cubic_point(axis, c, d, y_seed) - point;
    let chord_delta = smooth_cubic_point(axis, c, d, chord_seed) - point;
    var t = select(y_seed, chord_seed, dot(chord_delta, chord_delta) < dot(y_delta, y_delta));

    for (var iteration = 0u; iteration < 4u; iteration += 1u) {
        let curve_point = smooth_cubic_point(axis, c, d, t);
        let tangent = smooth_cubic_derivative(axis, c, d, t);
        let second_derivative = smooth_cubic_second_derivative(axis, c, d, t);
        let delta = curve_point - point;
        let denominator = dot(tangent, tangent) + dot(delta, second_derivative);
        if (abs(denominator) > SMOOTH_EPSILON) {
            t = clamp(t - dot(delta, tangent) / denominator, 0.0, 1.0);
        }
    }

    var closest: SmoothCubicPoint;
    closest.path_t = t;
    closest.point = smooth_cubic_point(axis, c, d, t);
    closest.distance = length(point - closest.point);

    let start_distance = length(point);
    if (start_distance < closest.distance) {
        closest.path_t = 0.0;
        closest.point = vec2<f32>(0.0);
        closest.distance = start_distance;
    }

    let end_distance = length(point - chord);
    if (end_distance < closest.distance) {
        closest.path_t = 1.0;
        closest.point = chord;
        closest.distance = end_distance;
    }

    closest.tangent = smooth_cubic_derivative(axis, c, d, closest.path_t);
    return closest;
}

fn smooth_signed_distance(delta: vec2<f32>, normal: vec2<f32>, distance: f32) -> f32 {
    return select(-distance, distance, dot(delta, normal) >= 0.0);
}

fn smooth_unfold_normal(normal: vec2<f32>, mirrored: bool) -> vec2<f32> {
    return select(vec2<f32>(normal.x, -normal.y), vec2<f32>(-normal.y, normal.x), mirrored);
}

fn smooth_corner_sample(corner_to_point: vec2<f32>, params: SmoothCornerParams) -> SmoothSample {
    var sample: SmoothSample;
    sample.corner = SMOOTH_NO_CORNER;
    let z = corner_to_point + vec2<f32>(params.horizontal.p, params.vertical.p);

    if (params.radius <= SMOOTH_EPSILON || z.x <= 0.0 || z.y <= 0.0) {
        sample.distance = max(corner_to_point.x, corner_to_point.y);
        sample.normal = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0),
                               corner_to_point.x > corner_to_point.y);
        sample.path_t = 0.0;
        sample.segment = SMOOTH_SEGMENT_STRAIGHT;
        return sample;
    }

    let horizontal_point = vec2<f32>(z.x, params.vertical.p - z.y);
    let vertical_point = vec2<f32>(z.y, params.horizontal.p - z.x);
    let circle_center = vec2<f32>(params.horizontal.p - params.radius, params.radius);
    let join = vec2<f32>(params.horizontal.a + params.horizontal.b + params.c, params.d);
    let start_direction = (join - circle_center) / params.radius;
    let to_point = horizontal_point - circle_center;
    let to_point_length = length(to_point);
    let point_direction = select(start_direction, to_point / max(to_point_length, SMOOTH_EPSILON),
                                 to_point_length > SMOOTH_EPSILON);

    let cross_z = start_direction.x * point_direction.y - start_direction.y * point_direction.x;
    let arc_angle = clamp(atan2(cross_z, dot(start_direction, point_direction)), 0.0,
                          params.arc_sweep);
    let arc_sine = sin(arc_angle);
    let arc_cosine = cos(arc_angle);
    let arc_normal = vec2<f32>(
        arc_cosine * start_direction.x - arc_sine * start_direction.y,
        arc_sine * start_direction.x + arc_cosine * start_direction.y,
    );
    let arc_delta = horizontal_point - (circle_center + params.radius * arc_normal);

    sample.distance = smooth_signed_distance(arc_delta, arc_normal, length(arc_delta));
    sample.normal = smooth_unfold_normal(arc_normal, false);
    sample.path_t = select(0.0, arc_angle / max(params.arc_sweep, SMOOTH_EPSILON),
                           params.arc_sweep > SMOOTH_EPSILON);
    sample.segment = SMOOTH_SEGMENT_ARC;

    if (params.smoothing > SMOOTH_EPSILON) {
        let horizontal_bounds_delta = max(vec2<f32>(0.0),
                                          max(-horizontal_point, horizontal_point - join));
        if (dot(horizontal_bounds_delta, horizontal_bounds_delta) <=
                sample.distance * sample.distance * 1.000001 + SMOOTH_EPSILON) {
            let cubic = smooth_closest_cubic(horizontal_point, params.horizontal, params.c,
                                             params.d);
            let normal = normalize(vec2<f32>(cubic.tangent.y, -cubic.tangent.x));
            let distance = smooth_signed_distance(horizontal_point - cubic.point, normal,
                                                  cubic.distance);
            if (abs(distance) <= abs(sample.distance)) {
                sample.distance = distance;
                sample.normal = smooth_unfold_normal(normal, false);
                sample.path_t = cubic.path_t;
                sample.segment = SMOOTH_SEGMENT_FIRST_CUBIC;
            }
        }

        let vertical_join = vec2<f32>(params.vertical.a + params.vertical.b + params.c, params.d);
        let vertical_bounds_delta = max(vec2<f32>(0.0),
                                        max(-vertical_point, vertical_point - vertical_join));
        if (dot(vertical_bounds_delta, vertical_bounds_delta) <=
                sample.distance * sample.distance * 1.000001 + SMOOTH_EPSILON) {
            let cubic = smooth_closest_cubic(vertical_point, params.vertical, params.c, params.d);
            let normal = normalize(vec2<f32>(cubic.tangent.y, -cubic.tangent.x));
            let distance = smooth_signed_distance(vertical_point - cubic.point, normal,
                                                  cubic.distance);
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

fn smooth_cubic_length(params: SmoothCornerParams, axis: SmoothAxis, end_t: f32) -> f32 {
    let half_t = clamp(end_t, 0.0, 1.0) / 2.0;
    if (half_t <= 0.0 || params.smoothing <= 0.0) {
        return 0.0;
    }
    let offset1 = half_t * 0.5384693101;
    let offset2 = half_t * 0.9061798459;
    let speed0 = length(smooth_cubic_derivative(axis, params.c, params.d, half_t));
    let speed1 = length(smooth_cubic_derivative(axis, params.c, params.d, half_t - offset1)) +
        length(smooth_cubic_derivative(axis, params.c, params.d, half_t + offset1));
    let speed2 = length(smooth_cubic_derivative(axis, params.c, params.d, half_t - offset2)) +
        length(smooth_cubic_derivative(axis, params.c, params.d, half_t + offset2));
    return half_t * (0.5688888889 * speed0 + 0.4786286705 * speed1 + 0.2369268851 * speed2);
}

fn smooth_corner_length(params: SmoothCornerParams) -> f32 {
    return smooth_cubic_length(params, params.horizontal, 1.0) +
        params.radius * params.arc_sweep +
        smooth_cubic_length(params, params.vertical, 1.0);
}

fn smooth_corner_progress(params: SmoothCornerParams, sample: SmoothSample,
                          total_length: f32) -> f32 {
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

fn smooth_is_corner_candidate(point: vec2<f32>, size: vec2<f32>, horizontal_extent: f32,
                              vertical_extent: f32, corner: u32) -> bool {
    if (horizontal_extent <= SMOOTH_EPSILON || vertical_extent <= SMOOTH_EPSILON) {
        return false;
    }
    if (corner == 0u) {
        return point.x <= horizontal_extent && point.y <= vertical_extent;
    }
    if (corner == 1u) {
        return size.x - point.x <= horizontal_extent && point.y <= vertical_extent;
    }
    if (corner == 2u) {
        return size.x - point.x <= horizontal_extent && size.y - point.y <= vertical_extent;
    }
    return point.x <= horizontal_extent && size.y - point.y <= vertical_extent;
}

fn smooth_has_corner_candidate(point: vec2<f32>, size: vec2<f32>, prepared: SmoothCorners) -> bool {
    for (var corner = 0u; corner < 4u; corner += 1u) {
        if (smooth_is_corner_candidate(point, size, prepared.horizontal_reaches[corner],
                                       prepared.vertical_reaches[corner], corner)) {
            return true;
        }
    }
    return false;
}

fn smooth_corner_to_point(point: vec2<f32>, size: vec2<f32>, corner: u32) -> vec2<f32> {
    if (corner == 0u) {
        return -point;
    }
    if (corner == 1u) {
        return vec2<f32>(point.x - size.x, -point.y);
    }
    if (corner == 2u) {
        return point - size;
    }
    return vec2<f32>(-point.x, point.y - size.y);
}

fn smooth_orient_normal(normal: vec2<f32>, corner: u32) -> vec2<f32> {
    if (corner == 0u) {
        return -normal;
    }
    if (corner == 1u) {
        return vec2<f32>(normal.x, -normal.y);
    }
    if (corner == 2u) {
        return normal;
    }
    return vec2<f32>(-normal.x, normal.y);
}

fn smooth_straight_sample(point: vec2<f32>, size: vec2<f32>, prepared: SmoothCorners) -> SmoothSample {
    let h = prepared.horizontal_reaches;
    let v = prepared.vertical_reaches;

    var nearest_delta = point - vec2<f32>(clamp(point.x, h.x, max(h.x, size.x - h.y)), 0.0);
    var nearest_normal = vec2<f32>(0.0, -1.0);
    var nearest_distance_squared = dot(nearest_delta, nearest_delta);

    var candidate_delta = point - vec2<f32>(size.x, clamp(point.y, v.y, max(v.y, size.y - v.z)));
    var candidate_distance_squared = dot(candidate_delta, candidate_delta);
    if (candidate_distance_squared < nearest_distance_squared) {
        nearest_delta = candidate_delta;
        nearest_normal = vec2<f32>(1.0, 0.0);
        nearest_distance_squared = candidate_distance_squared;
    }

    candidate_delta = point - vec2<f32>(clamp(point.x, h.w, max(h.w, size.x - h.z)), size.y);
    candidate_distance_squared = dot(candidate_delta, candidate_delta);
    if (candidate_distance_squared < nearest_distance_squared) {
        nearest_delta = candidate_delta;
        nearest_normal = vec2<f32>(0.0, 1.0);
        nearest_distance_squared = candidate_distance_squared;
    }

    candidate_delta = point - vec2<f32>(0.0, clamp(point.y, v.x, max(v.x, size.y - v.w)));
    candidate_distance_squared = dot(candidate_delta, candidate_delta);
    if (candidate_distance_squared < nearest_distance_squared) {
        nearest_delta = candidate_delta;
        nearest_normal = vec2<f32>(-1.0, 0.0);
        nearest_distance_squared = candidate_distance_squared;
    }

    var sample: SmoothSample;
    sample.distance = smooth_signed_distance(nearest_delta, nearest_normal,
                                             sqrt(nearest_distance_squared));
    sample.normal = nearest_normal;
    sample.path_t = 0.0;
    sample.segment = SMOOTH_SEGMENT_STRAIGHT;
    sample.corner = SMOOTH_NO_CORNER;
    return sample;
}

// `point` is relative to the rectangle's origin.
fn smooth_rect_sample(point: vec2<f32>, size: vec2<f32>, radii: vec4<f32>,
                      prepared: SmoothCorners) -> SmoothSample {
    var result = smooth_straight_sample(point, size, prepared);
    if (!smooth_has_corner_candidate(point, size, prepared)) {
        return result;
    }

    for (var corner = 0u; corner < 4u; corner += 1u) {
        if (smooth_is_corner_candidate(point, size, prepared.horizontal_reaches[corner],
                                       prepared.vertical_reaches[corner], corner)) {
            let params = smooth_corner_params(radii[corner], prepared.horizontal_reaches[corner],
                                              prepared.vertical_reaches[corner], prepared.factors);
            if (params.radius > SMOOTH_EPSILON) {
                var candidate = smooth_corner_sample(smooth_corner_to_point(point, size, corner),
                                                     params);
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

fn smooth_rect_sdf(point: vec2<f32>, bounds: Bounds, corner_radii: Corners,
                   corner_smoothing: f32) -> f32 {
    let radii = corners_to_vec4(corner_radii);
    let prepared = smooth_prepare(bounds.size, radii, corner_smoothing);
    return smooth_rect_sample(point - bounds.origin, bounds.size, radii, prepared).distance;
}
