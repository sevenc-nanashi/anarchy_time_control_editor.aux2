// NOTE:
// 最初の3点：
// - 点のX・Y座標は0.0に固定する。
//   - もし最初の3点が0.0ではない場合は通常のカーブとして扱う
//   - 逆に最初の3点が0.0であれば、これはカスタマイズされたカーブであるとみなして本格的に解析する
//     - 流石に最初の点に対して制御点を重ねるユーザーはいないと思う...
// - 4番目以降以降のすべての点のX・Y座標、ハンドルのX・Y座標をHASH_QUANTIZATION_UNIT_INVで掛けて、四捨五入する
//   それらを64bitの値に変換する
// - それらの64bitの値をxxh32にかけて、ハッシュ値を生成する
// - 1番目の点は座標(0.0, 0.0)、ハンドル(1.0, 0.0)で固定する。
// - 2番目の点のハンドルのX座標はハッシュ値の上位16bit、3番目の点のハンドルのX座標はハッシュ値の下位16bitに設定する。
// それ以降：
// - 各カーブ区間は3つのセクションで構成される：
//   - 2つの開始点。メタデータを埋め込む。
//   - データ点。カーブごとの情報を埋め込む。
//   - カーブ点。実際に計算されたカーブをAviUtl2用に変換したもの。
// - 開始点とデータ点のハンドルのY座標は0.0に固定し、X座標だけにデータを埋め込む。
// - 1つ目の開始点のハンドルのX座標[0.0, u16::MAX]はカーブ点の数を表す。
// - 2つ目の開始点のハンドルのX座標[0.0, u16::MAX]はビット列として扱う：
//     - 1、2ビット目：
//       - 00: Bezier
//       - 01: Elastic
//       - 10: Bounce
//       - 11: <Error>
//   - Bezier：
//     - 開始点3ビット目：左の点のハンドルが分離しているかどうか。
//     - データ点：
//       - 1、2：ハンドルのX座標を左側のハンドルのX・Y座標として読み取る。
//       - 3、4：ハンドルのX座標を右側のハンドルのX・Y座標として読み取る。
//     - カーブ点：
//       - 最初のカーブ点のX・Y座標を左側の点の位置として読み取る。
//       - 最後のカーブ点のX・Y座標を右側の点の位置として読み取る。
//    - Elastic：
//      - 開始点3ビット目：反転しているかどうか。
//      - データ点：
//        - 1：ハンドルのX座標をamplitudeとして読み取る。
//        - 2：ハンドルのX座標をfrequencyとして読み取る。
//        - 3：ハンドルのX座標をdecayとして読み取る。
//    - Bounce：
//      - 開始点3ビット目：反転しているかどうか。
//      - データ点：
//        - 1、2：ハンドルのX座標を頂点のX・Y座標として読み取る。

use aviutl2::anyhow::Context;

const XXH32_SEED: u32 = 0;

#[derive(Debug, Clone)]
pub struct ObjectEffectsInfo {
    pub handle: aviutl2::generic::ObjectHandle,
    pub effects: Vec<EffectTracksInfo>,
}

#[derive(Debug, Clone)]
pub struct EffectTracksInfo {
    pub name: String,
    pub handle: aviutl2::generic::EffectHandle,
    pub tracks: Vec<TrackInfo>,
}

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub track_name: Vec<String>,
    pub curve: Option<crate::curve::TimeControl>,
}

pub(crate) fn read_object_effects_info(
    read: &aviutl2::generic::ReadSection,
    object: aviutl2::generic::ObjectHandle,
) -> aviutl2::common::AnyResult<ObjectEffectsInfo> {
    let object = read.object(object);
    let effects = object.get_effects()?;
    let mut effects_info = vec![];
    for effect in effects {
        let effect = read.effect(effect);
        let effect_name = effect.get_name()?;
        let tracks = crate::EDIT_HANDLE.get_effect_items(&effect_name)?;
        let mut tracks_info = Vec::new();
        let mut registered_track_names = std::collections::HashSet::new();
        for track in tracks {
            if registered_track_names.contains(&track.name) {
                continue;
            }
            let Ok(track_info) = effect.get_track_info(&track.name) else {
                continue;
            };

            let track_names =
                match crate::EDIT_HANDLE.get_effect_item_group_names(&effect_name, &track.name)? {
                    Some(track_names) if track_info.is_none_or(|t| t.group_num > 1) => track_names,
                    _ => vec![track.name],
                };
            assert!(!track_names.is_empty(), "Track group must not be empty");
            registered_track_names.extend(track_names.iter().cloned());

            let primary_track_name = &track_names[0];
            let primary_track_info = effect.get_track_info(primary_track_name)?;
            let value = effect.get_item_value(primary_track_name)?;
            let parsed_track = aviutl2_track_parser::Track::parse(&value, &primary_track_info)?;
            let curve = match parsed_track.time_control_points {
                None => None,
                Some(time_control) => match parse_curve_from_track(&time_control) {
                    Ok(curve) => Some(curve),
                    Err(e) => {
                        tracing::warn!(
                            "Failed to read curve for track group {} of effect {}: {:?}",
                            track_names.join(", "),
                            effect_name,
                            e
                        );
                        None
                    }
                },
            };
            tracks_info.push(TrackInfo {
                track_name: track_names,
                curve,
            });
        }

        effects_info.push(EffectTracksInfo {
            name: effect_name,
            handle: effect.handle,
            tracks: tracks_info,
        });
    }

    Ok(ObjectEffectsInfo {
        handle: object.handle,
        effects: effects_info,
    })
}

pub fn write_curve_to_track(
    edit: &aviutl2::generic::EditSection,
    effect_handle: aviutl2::generic::EffectHandle,
    track_names: &[String],
    curve: &crate::curve::TimeControl,
) -> aviutl2::common::AnyResult<()> {
    let effect = edit.effect(effect_handle);
    let points = serialize_curve_to_points(curve)?;
    let Some((primary_track_name, other_track_names)) = track_names.split_first() else {
        unreachable!("Track group must not be empty");
    };

    static DEFAULT_CURVE_SCRIPT: std::sync::LazyLock<&'static str> =
        std::sync::LazyLock::new(|| {
            if crate::EDIT_HANDLE
                .get_modules()
                .iter()
                .find(|m| m.name == "区間ごとに時間制御@Basic_S")
                .is_some()
            {
                "区間ごとに時間制御@Basic_S"
            } else {
                "直線移動(時間制御)"
            }
        });

    let current_value = effect.get_item_value(primary_track_name)?;
    let track_info = effect.get_track_info(primary_track_name)?;
    let mut primary_track = aviutl2_track_parser::Track::parse(&current_value, &track_info)?;
    if primary_track.time_control_points.is_none() {
        primary_track.movement = Some(aviutl2_track_parser::Movement {
            name: DEFAULT_CURVE_SCRIPT.to_string(),
            parameters: vec![],
        });
    }
    primary_track.time_control_points = Some(points.clone());
    let new_value = primary_track.to_string();
    tracing::debug!(
        "Writing new value to track {} of effect {}: {}",
        primary_track_name,
        effect.get_name()?,
        new_value
    );
    effect.set_item_value(primary_track_name, &new_value)?;

    for track_name in other_track_names {
        let current_value = effect.get_item_value(track_name)?;
        let track_info = effect.get_track_info(track_name)?;
        let mut current_track = aviutl2_track_parser::Track::parse(&current_value, &track_info)?;
        current_track.time_control_points = primary_track.time_control_points.clone();
        current_track.flag = primary_track.flag;
        current_track.movement = primary_track.movement.clone();

        let new_value = current_track.to_string();
        tracing::debug!(
            "Writing new value to other track in group, {} of effect {}: {}",
            track_name,
            effect.get_name()?,
            new_value
        );
        effect.set_item_value(track_name, &new_value)?;
    }
    Ok(())
}

fn parse_curve_from_track(
    points: &[aviutl2_track_parser::TimeControlPoint],
) -> anyhow::Result<crate::curve::TimeControl> {
    if let Some(curve) = load_customized_curve_from_points(&points)? {
        return Ok(curve);
    }

    load_regular_curve_from_points(&points)
}

fn load_regular_curve_from_points(
    points: &[aviutl2_track_parser::TimeControlPoint],
) -> anyhow::Result<crate::curve::TimeControl> {
    anyhow::ensure!(
        points.len() >= 2,
        "A regular curve must contain at least two points"
    );

    let curve_points = points
        .iter()
        .map(|point| crate::curve::TimeControlPoint {
            position: point.coordinate.into(),
            handles_separated: false,
        })
        .collect();
    let segments = points
        .windows(2)
        .map(|points| {
            let start = &points[0];
            let end = &points[1];
            crate::curve::TimeControlSegment::Bezier(crate::curve::TimeControlBezier {
                start_handle: [
                    start.coordinate.0 + start.handle_offset.0,
                    start.coordinate.1 + start.handle_offset.1,
                ],
                end_handle: [
                    end.coordinate.0 - end.handle_offset.0,
                    end.coordinate.1 - end.handle_offset.1,
                ],
            })
        })
        .collect();

    Ok(crate::curve::TimeControl {
        points: curve_points,
        segments,
    })
}

static HASH_QUANTIZATION_UNIT_INV: f64 = 1e6;

#[derive(Clone, Copy)]
struct CubicBezierSpan {
    start: [f64; 2],
    start_handle: [f64; 2],
    end_handle: [f64; 2],
    end: [f64; 2],
}

struct ElasticFormula {
    amplitude: f64,
    frequency: f64,
    decay: f64,
    omega: f64,
    exp_k: f64,
    first_extremum_t: f64,
    first_extremum_value: f64,
}

impl ElasticFormula {
    fn new(elastic: &crate::curve::TimeControlElastic) -> anyhow::Result<Self> {
        let amplitude = elastic.amplitude.clamp(0.0, 1.0);
        let frequency = elastic.frequency.max(0.5);
        let decay = elastic.decay.max(1.0);
        anyhow::ensure!(
            frequency.ceil() * 4.0 <= u16::MAX as f64,
            "Elastic frequency produces too many curve points"
        );
        let omega = 2.0 * std::f64::consts::PI * frequency;
        let exp_k = (-decay).exp();
        let mut formula = Self {
            amplitude,
            frequency,
            decay,
            omega,
            exp_k,
            first_extremum_t: 0.0,
            first_extremum_value: 0.0,
        };
        let mut first_extremum_t = (0.5 - (decay / frequency).sqrt() * 0.05) / frequency;
        for _ in 0..3 {
            first_extremum_t -= formula.base_derivative_numerator(first_extremum_t)
                / formula.base_second_derivative_numerator(first_extremum_t);
        }
        formula.first_extremum_t = first_extremum_t;
        formula.first_extremum_value = formula.base_value(first_extremum_t);
        Ok(formula)
    }

    fn base_value(&self, progress: f64) -> f64 {
        let coef = (self.exp_k.powf(progress) - self.exp_k) / (1.0 - self.exp_k);
        1.0 - coef * (self.omega * progress).cos()
    }

    fn base_derivative_numerator(&self, progress: f64) -> f64 {
        let angle = self.omega * progress;
        let exp_kt = self.exp_k.powf(progress);
        self.decay * exp_kt * angle.cos() + self.omega * (exp_kt - self.exp_k) * angle.sin()
    }

    fn base_second_derivative_numerator(&self, progress: f64) -> f64 {
        let angle = self.omega * progress;
        let exp_kt = self.exp_k.powf(progress);
        let omega_sq = self.omega * self.omega;
        ((omega_sq - self.decay * self.decay) * exp_kt - omega_sq * self.exp_k) * angle.cos()
            - 2.0 * self.omega * self.decay * exp_kt * angle.sin()
    }

    fn value_and_derivative(&self, progress: f64) -> (f64, f64) {
        let value = self.base_value(progress);
        let derivative = self.base_derivative_numerator(progress) / (1.0 - self.exp_k);
        let (value, derivative) = if progress < self.first_extremum_t {
            let scale = (self.amplitude * (self.first_extremum_value - 1.0) + 1.0)
                / self.first_extremum_value;
            (scale * value, scale * derivative)
        } else {
            (
                self.amplitude * (value - 1.0) + 1.0,
                self.amplitude * derivative,
            )
        };
        if !(0.0..=2.0).contains(&value) {
            (value.clamp(0.0, 2.0), 0.0)
        } else {
            (value, derivative)
        }
    }

    fn extrema(&self) -> Vec<f64> {
        let divisions = (self.frequency * 8.0).ceil() as usize;
        let mut extrema = Vec::new();
        let mut previous_t = 0.0;
        let mut previous_derivative = self.base_derivative_numerator(previous_t);
        for index in 1..=divisions {
            let current_t = index as f64 / divisions as f64;
            let current_derivative = self.base_derivative_numerator(current_t);
            if previous_derivative == 0.0 && previous_t > 0.0 {
                extrema.push(previous_t);
            } else if previous_derivative.signum() != current_derivative.signum() {
                let mut min_t = previous_t;
                let mut max_t = current_t;
                let mut min_derivative = previous_derivative;
                for _ in 0..48 {
                    let middle_t = (min_t + max_t) / 2.0;
                    let middle_derivative = self.base_derivative_numerator(middle_t);
                    if min_derivative.signum() == middle_derivative.signum() {
                        min_t = middle_t;
                        min_derivative = middle_derivative;
                    } else {
                        max_t = middle_t;
                    }
                }
                extrema.push((min_t + max_t) / 2.0);
            }
            previous_t = current_t;
            previous_derivative = current_derivative;
        }
        extrema.dedup_by(|left, right| (*left - *right).abs() < 0.000_000_001);
        extrema.retain(|progress| *progress > 0.0 && *progress < 1.0);
        extrema
    }
}

struct BounceFormula {
    cor: f64,
    period: f64,
    active_end: f64,
}

impl BounceFormula {
    fn new(bounce: &crate::curve::TimeControlBounce) -> Self {
        let handle_x = bounce.vertex[0].clamp(0.001, 0.999);
        let handle_y = bounce.vertex[1].clamp(0.001, 1.0);
        let cor = (1.0 - handle_y).sqrt().clamp(0.001, 0.999);
        let period = 2.0 * handle_x / (cor + 1.0);
        let limit_value = period * (1.0 / (1.0 - cor) - 0.5);
        let active_end = if limit_value > 1.0 {
            let bounce_index = ((1.0 + (cor - 1.0) * (1.0 / period + 0.5)).ln() / cor.ln()).floor();
            period * ((cor.powf(bounce_index) - 1.0) / (cor - 1.0) - 0.5)
        } else {
            limit_value
        };
        Self {
            cor,
            period,
            active_end: active_end.clamp(0.0, 1.0),
        }
    }

    fn boundary(&self, bounce_index: usize) -> f64 {
        self.period * ((self.cor.powi(bounce_index as i32) - 1.0) / (self.cor - 1.0) - 0.5)
    }

    fn vertex(&self, bounce_index: usize) -> f64 {
        self.period
            * (-0.5 - 1.0 / (self.cor - 1.0)
                + (self.cor + 1.0) * self.cor.powi(bounce_index as i32) / (2.0 * self.cor - 2.0))
    }

    fn value_and_derivative(&self, bounce_index: usize, progress: f64) -> (f64, f64) {
        let local_progress = progress / self.period;
        let local_vertex = self.vertex(bounce_index) / self.period;
        let offset = local_progress - local_vertex;
        (
            1.0 + 4.0 * offset * offset - self.cor.powi((bounce_index * 2) as i32),
            8.0 * offset / self.period,
        )
    }
}

fn round_curve_points(points: &mut [aviutl2_track_parser::TimeControlPoint]) {
    let scale = HASH_QUANTIZATION_UNIT_INV;
    for point in points {
        for value in [
            &mut point.coordinate.0,
            &mut point.coordinate.1,
            &mut point.handle_offset.0,
            &mut point.handle_offset.1,
        ] {
            *value = (*value * scale).round() / scale;
        }
    }
}

fn curve_hash(points: &[aviutl2_track_parser::TimeControlPoint]) -> u32 {
    let mut hasher = xxhash_rust::xxh32::Xxh32::new(XXH32_SEED);
    let scale = HASH_QUANTIZATION_UNIT_INV;
    for point in points {
        for value in [
            point.coordinate.0,
            point.coordinate.1,
            point.handle_offset.0,
            point.handle_offset.1,
        ] {
            hasher.update(&((value * scale).round() as i64).to_le_bytes());
        }
    }
    hasher.digest()
}

fn encode_u16(value: u16) -> f64 {
    value as f64
}

fn hidden_point(x: f64, y: f64, value: f64) -> aviutl2_track_parser::TimeControlPoint {
    aviutl2_track_parser::TimeControlPoint {
        coordinate: (x, y),
        handle_offset: (value, 0.0),
    }
}

fn hermite_span(
    start_x: f64,
    start_y: f64,
    start_derivative: f64,
    end_x: f64,
    end_y: f64,
    end_derivative: f64,
) -> CubicBezierSpan {
    let width = end_x - start_x;
    CubicBezierSpan {
        start: [start_x, start_y],
        start_handle: [
            start_x + width / 3.0,
            start_y + start_derivative * width / 3.0,
        ],
        end_handle: [end_x - width / 3.0, end_y - end_derivative * width / 3.0],
        end: [end_x, end_y],
    }
}

fn reverse_spans(spans: Vec<CubicBezierSpan>) -> Vec<CubicBezierSpan> {
    let reverse = |point: [f64; 2]| [1.0 - point[0], 1.0 - point[1]];
    spans
        .into_iter()
        .rev()
        .map(|span| CubicBezierSpan {
            start: reverse(span.end),
            start_handle: reverse(span.end_handle),
            end_handle: reverse(span.start_handle),
            end: reverse(span.start),
        })
        .collect()
}

fn scale_span(
    span: CubicBezierSpan,
    segment_start: [f64; 2],
    segment_end: [f64; 2],
) -> CubicBezierSpan {
    let scale = |point: [f64; 2]| {
        [
            segment_start[0] + (segment_end[0] - segment_start[0]) * point[0],
            segment_start[1] + (segment_end[1] - segment_start[1]) * point[1],
        ]
    };
    CubicBezierSpan {
        start: scale(span.start),
        start_handle: scale(span.start_handle),
        end_handle: scale(span.end_handle),
        end: scale(span.end),
    }
}

fn spans_to_points(spans: Vec<CubicBezierSpan>) -> Vec<aviutl2_track_parser::TimeControlPoint> {
    spans
        .into_iter()
        .flat_map(|span| {
            [
                aviutl2_track_parser::TimeControlPoint {
                    coordinate: span.start.into(),
                    handle_offset: (
                        span.start_handle[0] - span.start[0],
                        span.start_handle[1] - span.start[1],
                    ),
                },
                aviutl2_track_parser::TimeControlPoint {
                    coordinate: span.end.into(),
                    handle_offset: (
                        span.end[0] - span.end_handle[0],
                        span.end[1] - span.end_handle[1],
                    ),
                },
            ]
        })
        .collect()
}

fn elastic_spans(
    elastic: &crate::curve::TimeControlElastic,
) -> anyhow::Result<Vec<CubicBezierSpan>> {
    let formula = ElasticFormula::new(elastic)?;
    let mut vertices = vec![0.0];
    vertices.extend(formula.extrema());
    vertices.push(1.0);
    let mut spans = Vec::with_capacity(vertices.len() - 1);
    for vertices in vertices.windows(2) {
        let start = vertices[0];
        let end = vertices[1];
        let (start_y, start_derivative) = formula.value_and_derivative(start);
        let (end_y, end_derivative) = formula.value_and_derivative(end);
        spans.push(hermite_span(
            start,
            start_y,
            start_derivative,
            end,
            end_y,
            end_derivative,
        ));
    }
    if elastic.reversed {
        Ok(reverse_spans(spans))
    } else {
        Ok(spans)
    }
}

fn bounce_spans(bounce: &crate::curve::TimeControlBounce) -> anyhow::Result<Vec<CubicBezierSpan>> {
    let formula = BounceFormula::new(bounce);
    let mut spans = Vec::new();
    let mut bounce_index = 0;
    loop {
        let left = formula.boundary(bounce_index).max(0.0);
        if left >= formula.active_end {
            break;
        }
        let vertex = formula.vertex(bounce_index);
        let right = formula.boundary(bounce_index + 1).min(formula.active_end);
        if vertex > left {
            let (left_y, left_derivative) = formula.value_and_derivative(bounce_index, left);
            let (vertex_y, vertex_derivative) = formula.value_and_derivative(bounce_index, vertex);
            spans.push(hermite_span(
                left,
                left_y,
                left_derivative,
                vertex,
                vertex_y,
                vertex_derivative,
            ));
        }
        if right > vertex {
            let (vertex_y, vertex_derivative) = formula.value_and_derivative(bounce_index, vertex);
            let (right_y, right_derivative) = formula.value_and_derivative(bounce_index, right);
            spans.push(hermite_span(
                vertex,
                vertex_y,
                vertex_derivative,
                right,
                right_y,
                right_derivative,
            ));
        }
        anyhow::ensure!(
            spans.len() * 2 <= u16::MAX as usize,
            "Bounce parameters produce too many curve points"
        );
        if right >= formula.active_end {
            break;
        }
        if formula.active_end - right <= 1.0 / HASH_QUANTIZATION_UNIT_INV
            || formula.cor.powi((bounce_index * 2) as i32) < 1.0 / HASH_QUANTIZATION_UNIT_INV
        {
            break;
        }
        bounce_index += 1;
    }
    let represented_end = spans
        .last()
        .context("Bounce curve did not produce any spans")?
        .end[0];
    if represented_end < 1.0 {
        spans.push(hermite_span(represented_end, 1.0, 0.0, 1.0, 1.0, 0.0));
    }
    if bounce.reversed {
        Ok(reverse_spans(spans))
    } else {
        Ok(spans)
    }
}

fn serialize_segment_curve_points(
    segment: &crate::curve::TimeControlSegment,
    start: [f64; 2],
    end: [f64; 2],
) -> anyhow::Result<Vec<aviutl2_track_parser::TimeControlPoint>> {
    let spans = match segment {
        crate::curve::TimeControlSegment::Bezier(bezier) => vec![CubicBezierSpan {
            start,
            start_handle: bezier.start_handle,
            end_handle: bezier.end_handle,
            end,
        }],
        crate::curve::TimeControlSegment::Elastic(elastic) => elastic_spans(elastic)?
            .into_iter()
            .map(|span| scale_span(span, start, end))
            .collect(),
        crate::curve::TimeControlSegment::Bounce(bounce) => bounce_spans(bounce)?
            .into_iter()
            .map(|span| scale_span(span, start, end))
            .collect(),
    };
    let points = spans_to_points(spans);
    anyhow::ensure!(
        points.len() >= 2 && points.len() <= u16::MAX as usize,
        "Curve segment point count is outside the encodable range"
    );
    Ok(points)
}

fn serialize_curve_to_points(
    curve: &crate::curve::TimeControl,
) -> anyhow::Result<Vec<aviutl2_track_parser::TimeControlPoint>> {
    anyhow::ensure!(
        !curve.segments.is_empty(),
        "A customized curve must contain at least one segment"
    );
    anyhow::ensure!(
        curve.points.len() == curve.segments.len() + 1,
        "Curve points and segments are inconsistent"
    );

    let mut payload = Vec::new();
    for (segment_index, segment) in curve.segments.iter().enumerate() {
        let start = curve.points[segment_index].position;
        let end = curve.points[segment_index + 1].position;
        let curve_points = serialize_segment_curve_points(segment, start, end)?;
        let point_count = u16::try_from(curve_points.len())
            .context("Curve segment contains too many points for its metadata")?;
        let (mode_bits, third_bit) = match segment {
            crate::curve::TimeControlSegment::Bezier(_) => {
                (0b00, curve.points[segment_index].handles_separated)
            }
            crate::curve::TimeControlSegment::Elastic(elastic) => (0b01, elastic.reversed),
            crate::curve::TimeControlSegment::Bounce(bounce) => (0b10, bounce.reversed),
        };
        let bitflags = mode_bits | if third_bit { 0b100 } else { 0 };
        payload.push(hidden_point(start[0], start[1], encode_u16(point_count)));
        payload.push(hidden_point(start[0], start[1], encode_u16(bitflags)));
        match segment {
            crate::curve::TimeControlSegment::Bezier(bezier) => {
                payload.push(hidden_point(start[0], start[1], bezier.start_handle[0]));
                payload.push(hidden_point(start[0], start[1], bezier.start_handle[1]));
                payload.push(hidden_point(start[0], start[1], bezier.end_handle[0]));
                payload.push(hidden_point(start[0], start[1], bezier.end_handle[1]));
            }
            crate::curve::TimeControlSegment::Elastic(elastic) => {
                payload.push(hidden_point(start[0], start[1], elastic.amplitude));
                payload.push(hidden_point(start[0], start[1], elastic.frequency));
                payload.push(hidden_point(start[0], start[1], elastic.decay));
            }
            crate::curve::TimeControlSegment::Bounce(bounce) => {
                payload.push(hidden_point(start[0], start[1], bounce.vertex[0]));
                payload.push(hidden_point(start[0], start[1], bounce.vertex[1]));
            }
        }
        payload.extend(curve_points);
    }

    round_curve_points(&mut payload);
    let hash = curve_hash(&payload);
    tracing::debug!(
        "Serialized curve with {} segments and {} points, hash: {hash:#x}",
        curve.segments.len(),
        payload.len(),
    );
    let mut points = vec![
        aviutl2_track_parser::TimeControlPoint {
            coordinate: (0.0, 0.0),
            handle_offset: (1.0, 0.0),
        },
        aviutl2_track_parser::TimeControlPoint {
            coordinate: (0.0, 0.0),
            handle_offset: (encode_u16((hash >> 16) as u16), 0.0),
        },
        aviutl2_track_parser::TimeControlPoint {
            coordinate: (0.0, 0.0),
            handle_offset: (encode_u16((hash & 0xFFFF) as u16), 0.0),
        },
    ];
    points.extend(payload);
    Ok(points)
}

fn load_customized_curve_from_points(
    points: &[aviutl2_track_parser::TimeControlPoint],
) -> anyhow::Result<Option<crate::curve::TimeControl>> {
    if points.len() < 3
        || points[0].coordinate != (0.0, 0.0)
        || points[0].handle_offset != (1.0, 0.0)
        || points[1].coordinate != (0.0, 0.0)
        || points[2].coordinate != (0.0, 0.0)
    {
        return Ok(None);
    }

    let hash_value = curve_hash(&points[3..]);

    let quantized_first_handle_x = points[1].handle_offset.0.round() as u16;
    let quantized_second_handle_x = points[2].handle_offset.0.round() as u16;
    let curve_hash_value =
        ((quantized_first_handle_x as u32) << 16) | (quantized_second_handle_x as u32);

    if hash_value != curve_hash_value {
        anyhow::bail!("Curve hash mismatch: expected {curve_hash_value:#x}, got {hash_value:#x}");
    }

    let mut curve = crate::curve::TimeControl {
        points: Vec::new(),
        segments: Vec::new(),
    };
    let mut cursor = 3;
    while cursor < points.len() {
        let start_point = &points[cursor];
        cursor += 1;
        let bitflags_point = points
            .get(cursor)
            .context("Curve segment does not contain its flags")?;
        cursor += 1;
        let point_count = start_point.handle_offset.0.round() as usize;
        let bitflags = bitflags_point.handle_offset.0.round() as u16;
        let mode = match bitflags & 0b11 {
            0b00 => crate::curve::TimeControlMode::Bezier,
            0b01 => crate::curve::TimeControlMode::Elastic,
            0b10 => crate::curve::TimeControlMode::Bounce,
            _ => anyhow::bail!("Invalid handle type in curve data"),
        };
        let third_bit = bitflags & 0b100 != 0;

        let segment = match mode {
            crate::curve::TimeControlMode::Bezier => {
                let start_handle_x = points
                    .get(cursor)
                    .context("Bezier segment does not contain its start handle X")?;
                let start_handle_y = points
                    .get(cursor + 1)
                    .context("Bezier segment does not contain its start handle Y")?;
                let end_handle_x = points
                    .get(cursor + 2)
                    .context("Bezier segment does not contain its end handle X")?;
                let end_handle_y = points
                    .get(cursor + 3)
                    .context("Bezier segment does not contain its end handle Y")?;
                cursor += 4;
                crate::curve::TimeControlSegment::Bezier(crate::curve::TimeControlBezier {
                    start_handle: [
                        start_handle_x.handle_offset.0,
                        start_handle_y.handle_offset.0,
                    ],
                    end_handle: [end_handle_x.handle_offset.0, end_handle_y.handle_offset.0],
                })
            }
            crate::curve::TimeControlMode::Elastic => {
                let amplitude = points
                    .get(cursor)
                    .context("Elastic segment does not contain amplitude")?;
                let frequency = points
                    .get(cursor + 1)
                    .context("Elastic segment does not contain frequency")?;
                let decay = points
                    .get(cursor + 2)
                    .context("Elastic segment does not contain decay")?;
                cursor += 3;
                crate::curve::TimeControlSegment::Elastic(crate::curve::TimeControlElastic {
                    reversed: third_bit,
                    amplitude: amplitude.handle_offset.0,
                    frequency: frequency.handle_offset.0,
                    decay: decay.handle_offset.0,
                })
            }
            crate::curve::TimeControlMode::Bounce => {
                let vertex_x = points
                    .get(cursor)
                    .context("Bounce segment does not contain its vertex X")?;
                let vertex_y = points
                    .get(cursor + 1)
                    .context("Bounce segment does not contain its vertex Y")?;
                cursor += 2;
                crate::curve::TimeControlSegment::Bounce(crate::curve::TimeControlBounce {
                    reversed: third_bit,
                    vertex: [vertex_x.handle_offset.0, vertex_y.handle_offset.0],
                })
            }
        };

        anyhow::ensure!(
            point_count >= 2,
            "A curve segment must contain at least two sampled points"
        );
        let curve_points_end = cursor
            .checked_add(point_count)
            .context("Curve point count overflowed")?;
        let sampled_points = points
            .get(cursor..curve_points_end)
            .context("Curve segment does not contain the declared number of sampled points")?;
        cursor = curve_points_end;

        let segment_start = sampled_points
            .first()
            .context("Curve segment does not contain a start point")?
            .coordinate
            .into();
        let segment_end = sampled_points
            .last()
            .context("Curve segment does not contain an end point")?
            .coordinate
            .into();
        let handles_separated = matches!(mode, crate::curve::TimeControlMode::Bezier) && third_bit;

        if let Some(previous_end) = curve.points.last_mut() {
            anyhow::ensure!(
                previous_end.position == segment_start,
                "Adjacent curve segments do not share an endpoint"
            );
            previous_end.handles_separated = handles_separated;
        } else {
            curve.points.push(crate::curve::TimeControlPoint {
                position: segment_start,
                handles_separated,
            });
        }
        curve.points.push(crate::curve::TimeControlPoint {
            position: segment_end,
            handles_separated: false,
        });
        curve.segments.push(segment);
    }

    anyhow::ensure!(
        !curve.segments.is_empty(),
        "Customized curve does not contain any segments"
    );
    Ok(Some(curve))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(
        coordinate: [f64; 2],
        handle_offset: [f64; 2],
    ) -> aviutl2_track_parser::TimeControlPoint {
        aviutl2_track_parser::TimeControlPoint {
            coordinate: coordinate.into(),
            handle_offset: handle_offset.into(),
        }
    }

    fn assert_point_near(actual: [f64; 2], expected: [f64; 2]) {
        assert!((actual[0] - expected[0]).abs() < 0.000_001);
        assert!((actual[1] - expected[1]).abs() < 0.000_001);
    }

    fn cubic_point(span: CubicBezierSpan, t: f64) -> [f64; 2] {
        let mt = 1.0 - t;
        [
            mt.powi(3) * span.start[0]
                + 3.0 * mt.powi(2) * t * span.start_handle[0]
                + 3.0 * mt * t.powi(2) * span.end_handle[0]
                + t.powi(3) * span.end[0],
            mt.powi(3) * span.start[1]
                + 3.0 * mt.powi(2) * t * span.start_handle[1]
                + 3.0 * mt * t.powi(2) * span.end_handle[1]
                + t.powi(3) * span.end[1],
        ]
    }

    fn hidden_data(coordinate: [f64; 2], value: f64) -> aviutl2_track_parser::TimeControlPoint {
        point(coordinate, [value, 0.0])
    }

    fn with_customized_curve_header(
        payload: Vec<aviutl2_track_parser::TimeControlPoint>,
    ) -> Vec<aviutl2_track_parser::TimeControlPoint> {
        let mut hasher = xxhash_rust::xxh32::Xxh32::new(XXH32_SEED);
        let scale = HASH_QUANTIZATION_UNIT_INV;
        for point in &payload {
            for value in [
                point.coordinate.0,
                point.coordinate.1,
                point.handle_offset.0,
                point.handle_offset.1,
            ] {
                hasher.update(&((value * scale).round() as i64).to_le_bytes());
            }
        }
        let hash = hasher.digest();
        let hash_first = point([0.0, 0.0], [((hash >> 16) as u16) as f64, 0.0]);
        let hash_second = point([0.0, 0.0], [(hash as u16) as f64, 0.0]);

        let mut points = vec![point([0.0, 0.0], [1.0, 0.0]), hash_first, hash_second];
        points.extend(payload);
        points
    }

    #[test]
    fn regular_points_are_deserialized_as_linked_bezier_segments() {
        let points = vec![
            point([0.0, 0.0], [0.2, 0.3]),
            point([0.4, 0.5], [0.1, 0.2]),
            point([1.0, 1.0], [0.25, -0.1]),
        ];

        assert!(
            load_customized_curve_from_points(&points)
                .expect("regular points must be accepted")
                .is_none()
        );
        let curve =
            load_regular_curve_from_points(&points).expect("regular points must be deserialized");

        assert_eq!(curve.points.len(), 3);
        assert_eq!(curve.segments.len(), 2);
        assert_eq!(curve.points[1].position, [0.4, 0.5]);
        assert!(!curve.points.iter().any(|point| point.handles_separated));
        let Some(first) = curve.segment_bezier(0) else {
            panic!("first segment must be Bezier");
        };
        assert_point_near(first.start_handle, [0.2, 0.3]);
        assert_point_near(first.end_handle, [0.3, 0.3]);
        let Some(second) = curve.segment_bezier(1) else {
            panic!("second segment must be Bezier");
        };
        assert_point_near(second.start_handle, [0.5, 0.7]);
        assert_point_near(second.end_handle, [0.75, 1.1]);
    }

    #[test]
    fn customized_points_are_deserialized_by_segment_mode() {
        let payload = vec![
            hidden_data([0.0, 0.0], 2.0),
            hidden_data([0.0, 0.0], 0b100 as f64),
            hidden_data([0.0, 0.0], 0.1),
            hidden_data([0.0, 0.0], 0.2),
            hidden_data([0.0, 0.0], 0.3),
            hidden_data([0.0, 0.0], 0.4),
            point([0.0, 0.0], [0.1, 0.0]),
            point([0.4, 0.3], [0.1, 0.0]),
            hidden_data([0.4, 0.3], 2.0),
            hidden_data([0.4, 0.3], 0b101 as f64),
            hidden_data([0.4, 0.3], 0.6),
            hidden_data([0.4, 0.3], 2.5),
            hidden_data([0.4, 0.3], 3.0),
            point([0.4, 0.3], [0.1, 0.0]),
            point([0.7, 0.8], [0.1, 0.0]),
            hidden_data([0.7, 0.8], 2.0),
            hidden_data([0.7, 0.8], 0b010 as f64),
            hidden_data([0.7, 0.8], 0.55),
            hidden_data([0.7, 0.8], 0.75),
            point([0.7, 0.8], [0.1, 0.0]),
            point([1.0, 1.0], [0.1, 0.0]),
        ];
        let points = with_customized_curve_header(payload);

        let curve = load_customized_curve_from_points(&points)
            .expect("customized points must be accepted")
            .expect("customized points must be detected");

        assert_eq!(curve.points.len(), 4);
        assert_eq!(curve.segments.len(), 3);
        assert_eq!(curve.points[0].position, [0.0, 0.0]);
        assert_eq!(curve.points[1].position, [0.4, 0.3]);
        assert_eq!(curve.points[2].position, [0.7, 0.8]);
        assert_eq!(curve.points[3].position, [1.0, 1.0]);
        assert!(curve.points[0].handles_separated);
        assert!(!curve.points[1].handles_separated);

        let Some(bezier) = curve.segment_bezier(0) else {
            panic!("first segment must be Bezier");
        };
        assert_eq!(bezier.start_handle, [0.1, 0.2]);
        assert_eq!(bezier.end_handle, [0.3, 0.4]);
        let Some(crate::curve::TimeControlSegment::Elastic(elastic)) = curve.segments.get(1) else {
            panic!("second segment must be Elastic");
        };
        assert!(elastic.reversed);
        assert_eq!(elastic.amplitude, 0.6);
        assert_eq!(elastic.frequency, 2.5);
        assert_eq!(elastic.decay, 3.0);
        let Some(crate::curve::TimeControlSegment::Bounce(bounce)) = curve.segments.get(2) else {
            panic!("third segment must be Bounce");
        };
        assert!(!bounce.reversed);
        assert_eq!(bounce.vertex, [0.55, 0.75]);
    }

    #[test]
    fn customized_points_reject_truncated_sampled_points() {
        let points = with_customized_curve_header(vec![
            hidden_data([0.0, 0.0], 3.0),
            hidden_data([0.0, 0.0], 0b010 as f64),
            hidden_data([0.0, 0.0], 0.55),
            hidden_data([0.0, 0.0], 0.75),
            point([0.0, 0.0], [0.1, 0.0]),
            point([1.0, 1.0], [0.1, 0.0]),
        ]);

        let error = load_customized_curve_from_points(&points)
            .expect_err("truncated sampled points must be rejected");

        assert!(
            error
                .to_string()
                .contains("declared number of sampled points")
        );
    }

    #[test]
    fn customized_points_reject_missing_second_start_point() {
        let points = with_customized_curve_header(vec![hidden_data([0.0, 0.0], 2.0)]);

        let error = load_customized_curve_from_points(&points)
            .expect_err("a segment without its flags point must be rejected");

        assert!(error.to_string().contains("does not contain its flags"));
    }

    #[test]
    fn bezier_segment_is_serialized_with_its_original_handles() {
        let curve = crate::curve::TimeControl {
            points: vec![
                crate::curve::TimeControlPoint {
                    position: [0.0, 0.0],
                    handles_separated: true,
                },
                crate::curve::TimeControlPoint {
                    position: [1.0, 1.0],
                    handles_separated: false,
                },
            ],
            segments: vec![crate::curve::TimeControlSegment::Bezier(
                crate::curve::TimeControlBezier {
                    start_handle: [0.2, -0.3],
                    end_handle: [0.8, 1.4],
                },
            )],
        };

        let serialized = serialize_curve_to_points(&curve).expect("curve must be serialized");

        assert_eq!(serialized.len(), 11);
        assert!(
            serialized[3..9]
                .iter()
                .all(|point| point.handle_offset.1 == 0.0)
        );
        assert_point_near(serialized[9].coordinate.into(), [0.0, 0.0]);
        assert_point_near(serialized[9].handle_offset.into(), [0.2, -0.3]);
        assert_point_near(serialized[10].coordinate.into(), [1.0, 1.0]);
        assert_point_near(serialized[10].handle_offset.into(), [0.2, -0.4]);
        let restored = load_customized_curve_from_points(&serialized)
            .expect("serialized curve must pass validation")
            .expect("serialized curve must be customized");
        assert!(restored.points[0].handles_separated);
        let Some(bezier) = restored.segment_bezier(0) else {
            panic!("restored segment must be Bezier");
        };
        assert_point_near(bezier.start_handle, [0.2, -0.3]);
        assert_point_near(bezier.end_handle, [0.8, 1.4]);
    }

    #[test]
    fn serialized_hidden_points_do_not_use_y_handles() {
        for (mode, hidden_point_count) in [
            (crate::curve::TimeControlMode::Bezier, 6),
            (crate::curve::TimeControlMode::Elastic, 5),
            (crate::curve::TimeControlMode::Bounce, 4),
        ] {
            let curve = crate::curve::TimeControl::default_for_mode(mode);
            let serialized = serialize_curve_to_points(&curve).expect("curve must be serialized");
            let hidden_points = &serialized[3..3 + hidden_point_count];

            assert!(
                hidden_points
                    .iter()
                    .all(|point| point.handle_offset.1 == 0.0)
            );
        }
    }

    #[test]
    fn elastic_spans_put_horizontal_handles_at_extrema() {
        let elastic = crate::curve::TimeControlElastic::default();
        let spans = elastic_spans(&elastic).expect("elastic curve must be converted");
        let curve =
            crate::curve::TimeControl::default_for_mode(crate::curve::TimeControlMode::Elastic);

        assert!(spans.len() > 2);
        for span in &spans {
            assert!((span.start[1] - curve.y_at_x(span.start[0])).abs() < 0.000_001);
            assert!((span.end[1] - curve.y_at_x(span.end[0])).abs() < 0.000_001);
        }
        for spans in spans.windows(2) {
            assert_point_near(spans[0].end, spans[1].start);
            assert!((spans[0].end_handle[1] - spans[0].end[1]).abs() < 0.000_001);
            assert!((spans[1].start_handle[1] - spans[1].start[1]).abs() < 0.000_001);
        }
    }

    #[test]
    fn bounce_spans_follow_each_parabola_and_keep_contacts_sharp() {
        let bounce = crate::curve::TimeControlBounce::default();
        let spans = bounce_spans(&bounce).expect("bounce curve must be converted");
        let curve =
            crate::curve::TimeControl::default_for_mode(crate::curve::TimeControlMode::Bounce);

        for span in &spans {
            let middle = cubic_point(*span, 0.5);
            assert!((middle[1] - curve.y_at_x(middle[0])).abs() < 0.000_001);
        }
        let contact = spans
            .windows(2)
            .find(|spans| {
                (spans[0].end[0] - spans[1].start[0]).abs() < 0.000_001
                    && (spans[0].end[1] - 1.0).abs() < 0.000_001
                    && spans[0].end[0] < 0.999
            })
            .expect("bounce curve must contain a contact point");
        assert!(contact[0].end_handle[1] < contact[0].end[1]);
        assert!(contact[1].start_handle[1] < contact[1].start[1]);
    }

    #[test]
    fn mixed_curve_round_trips_through_serialized_points() {
        let curve = crate::curve::TimeControl {
            points: vec![
                crate::curve::TimeControlPoint {
                    position: [0.0, 0.0],
                    handles_separated: true,
                },
                crate::curve::TimeControlPoint {
                    position: [0.3, 0.4],
                    handles_separated: false,
                },
                crate::curve::TimeControlPoint {
                    position: [0.7, 0.6],
                    handles_separated: false,
                },
                crate::curve::TimeControlPoint {
                    position: [1.0, 1.0],
                    handles_separated: false,
                },
            ],
            segments: vec![
                crate::curve::TimeControlSegment::Bezier(crate::curve::TimeControlBezier {
                    start_handle: [0.1, 0.2],
                    end_handle: [0.2, 0.35],
                }),
                crate::curve::TimeControlSegment::Elastic(crate::curve::TimeControlElastic {
                    reversed: true,
                    amplitude: 0.7,
                    frequency: 3.5,
                    decay: 4.0,
                }),
                crate::curve::TimeControlSegment::Bounce(crate::curve::TimeControlBounce {
                    reversed: false,
                    vertex: [0.6, 0.8],
                }),
            ],
        };

        let serialized = serialize_curve_to_points(&curve).expect("curve must be serialized");
        let restored = load_customized_curve_from_points(&serialized)
            .expect("serialized curve must pass validation")
            .expect("serialized curve must be customized");

        assert_eq!(restored.points.len(), curve.points.len());
        assert_eq!(restored.segments.len(), curve.segments.len());
        for (actual, expected) in restored.points.iter().zip(&curve.points) {
            assert_point_near(actual.position, expected.position);
        }
        let Some(bezier) = restored.segment_bezier(0) else {
            panic!("first segment must be Bezier");
        };
        assert_point_near(bezier.start_handle, [0.1, 0.2]);
        assert_point_near(bezier.end_handle, [0.2, 0.35]);
        let Some(crate::curve::TimeControlSegment::Elastic(elastic)) = restored.segments.get(1)
        else {
            panic!("second segment must be Elastic");
        };
        assert!(elastic.reversed);
        assert_eq!(elastic.amplitude, 0.7);
        assert_eq!(elastic.frequency, 3.5);
        assert_eq!(elastic.decay, 4.0);
        let Some(crate::curve::TimeControlSegment::Bounce(bounce)) = restored.segments.get(2)
        else {
            panic!("third segment must be Bounce");
        };
        assert!(!bounce.reversed);
        assert_eq!(bounce.vertex, [0.6, 0.8]);
    }

    #[test]
    fn serialized_points_are_protected_by_the_curve_hash() {
        let mut serialized = serialize_curve_to_points(&crate::curve::TimeControl::default())
            .expect("curve must be serialized");
        serialized
            .last_mut()
            .expect("serialized curve must contain points")
            .coordinate
            .1 += 0.01;

        let error = load_customized_curve_from_points(&serialized)
            .expect_err("modified curve points must fail hash validation");

        assert!(error.to_string().contains("Curve hash mismatch"));
    }

    #[test]
    fn function_segment_spans_are_reversed_with_their_handles() {
        let normal_elastic = crate::curve::TimeControlElastic::default();
        let mut reversed_elastic = normal_elastic.clone();
        reversed_elastic.reversed = true;
        let normal = elastic_spans(&normal_elastic).expect("elastic curve must be converted");
        let reversed =
            elastic_spans(&reversed_elastic).expect("reversed elastic curve must be converted");

        assert_eq!(normal.len(), reversed.len());
        for (normal, reversed) in normal.iter().zip(reversed.iter().rev()) {
            assert_point_near(reversed.start, [1.0 - normal.end[0], 1.0 - normal.end[1]]);
            assert_point_near(
                reversed.start_handle,
                [1.0 - normal.end_handle[0], 1.0 - normal.end_handle[1]],
            );
            assert_point_near(
                reversed.end_handle,
                [1.0 - normal.start_handle[0], 1.0 - normal.start_handle[1]],
            );
            assert_point_near(reversed.end, [1.0 - normal.start[0], 1.0 - normal.start[1]]);
        }
    }

    #[test]
    fn serialization_rejects_inconsistent_curve_structure() {
        let curve = crate::curve::TimeControl {
            points: vec![crate::curve::TimeControlPoint {
                position: [0.0, 0.0],
                handles_separated: false,
            }],
            segments: vec![crate::curve::TimeControlSegment::Bezier(
                crate::curve::TimeControlBezier {
                    start_handle: [0.2, 0.2],
                    end_handle: [0.8, 0.8],
                },
            )],
        };

        let error = serialize_curve_to_points(&curve)
            .err()
            .expect("inconsistent curve must be rejected");

        assert!(error.to_string().contains("inconsistent"));
    }
}
