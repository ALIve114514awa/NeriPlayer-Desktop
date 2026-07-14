precision highp float;

uniform vec2 u_resolution;
uniform float u_time;
uniform float u_musicLevel;
uniform float u_beat;
uniform vec4 u_color0;
uniform vec4 u_color1;
uniform vec4 u_color2;
uniform vec4 u_color3;
uniform vec4 u_color4;
uniform float u_darkMode;
uniform float u_lightOffset;
uniform float u_saturateOffset;

// --- 色彩空间工具（对齐 Android hyper_background_effect.glsl） ---

vec3 rgb2hsv(vec3 c) {
  vec4 K = vec4(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
  vec4 p = mix(vec4(c.bg, K.wz), vec4(c.gb, K.xy), step(c.b, c.g));
  vec4 q = mix(vec4(p.xyw, c.r), vec4(c.r, p.yzx), step(p.x, c.r));
  float d = q.x - min(q.w, q.y);
  float e = 1.0e-10;
  return vec3(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

vec3 hsv2rgb(vec3 c) {
  vec4 K = vec4(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
  vec3 p = abs(fract(c.xxx + K.xyz) * 6.0 - K.www);
  return c.z * mix(K.xxx, clamp(p - K.xxx, 0.0, 1.0), c.y);
}

// Perlin 噪声（对齐 Android）
float hash(vec2 p) {
  vec3 p3 = fract(vec3(p.xyx) * 0.13);
  p3 += dot(p3, p3.yzx + 3.333);
  return fract((p3.x + p3.y) * p3.z);
}

float perlin(vec2 x) {
  vec2 i = floor(x);
  vec2 f = fract(x);
  float a = hash(i);
  float b = hash(i + vec2(1.0, 0.0));
  float c = hash(i + vec2(0.0, 1.0));
  float d = hash(i + vec2(1.0, 1.0));
  vec2 u = f * f * (3.0 - 2.0 * f);
  return mix(a, b, u.x) + (c - a) * u.y * (1.0 - u.x) + (d - b) * u.x * u.y;
}

// 颗粒抖动（对齐 Android gradientNoise）
float gradientNoise(vec2 uv) {
  return fract(52.9829189 * fract(dot(uv, vec2(0.06711056, 0.00583715))));
}

void main() {
  vec2 vUv = gl_FragCoord.xy / u_resolution;

  // 音频响应缩放（对齐 Android uZoom）
  float level = clamp(u_musicLevel, 0.0, 1.0);
  float beat = clamp(u_beat, 0.0, 1.0);
  float zoom = 1.0 + 0.024 * level + 0.105 * beat;
  vec2 center = vec2(0.5);
  vec2 uv = (vUv - center) / zoom + center;

  // Beat wave UV 畸变（对齐 Android：正弦波纹 + 径向脉冲 + 丝带波）
  uv += beat * 0.008 * sin(uv.yx * 8.0 + u_time * 4.0);
  float rd = length(uv - 0.5);
  uv += beat * 0.006 * normalize(uv - 0.5 + 1e-5) * sin(rd * 12.0 - u_time * 6.0);
  float motionEase = clamp(0.42 * level + 0.82 * beat, 0.0, 1.0);
  uv += motionEase * 0.005 * sin(vec2(uv.x + uv.y, uv.x - uv.y) * 6.0 + u_time * 2.0);

  // Beat 抖动（高频微扰）
  uv += (beat * beat) * 0.004 * vec2(sin(u_time * 60.0), cos(u_time * 54.0));

  // Perlin 噪声（对齐 Android uNoiseScale=1.5）
  float noiseValue = perlin(vUv * 1.5 + vec2(-u_time * 0.3, -u_time * 0.2));

  // 圆运动参数（对齐 Android）
  float pointOffset = 0.1 + 0.02 * level + 0.05 * beat;
  float pointRadiusMulti = 1.0 + 0.05 * level + 0.12 * beat;

  // 5 个点的位置和半径（对齐 Android 5 blob 配置）
  vec3 points[5];
  points[0] = vec3(0.63, 0.50, 0.88);
  points[1] = vec3(0.69, 0.75, 0.80);
  points[2] = vec3(0.17, 0.66, 0.81);
  points[3] = vec3(0.14, 0.24, 0.72);
  points[4] = vec3(0.50, 0.38, 0.76);

  vec4 colors[5];
  colors[0] = u_color0;
  colors[1] = u_color1;
  colors[2] = u_color2;
  colors[3] = u_color3;
  colors[4] = u_color4;

  // 混色循环（对齐 Android smoothstep 圆混合 + Lissajous 轨道）
  vec4 color = vec4(0.0);

  for (int i = 0; i < 5; i++) {
    vec4 pointColor = colors[i];
    pointColor.rgb *= pointColor.a;
    vec2 point = points[i].xy;
    float rad = points[i].z * pointRadiusMulti;

    // Lissajous 轨道 + beat 径向推力（对齐 Android updateAnimatedPoints）
    float phase = float(i) * 1.5708;
    point.x += sin(u_time * 0.7 + phase + point.y * 2.0) * pointOffset;
    point.y += cos(u_time * 0.5 + phase + point.x * 3.0) * pointOffset * 0.8;
    float pushDir = atan(point.y - 0.5, point.x - 0.5);
    point.xy += beat * 0.04 * vec2(cos(pushDir), sin(pushDir));

    float d = distance(uv, point);
    float pct = smoothstep(rad, 0.0, d);

    color.rgb = mix(color.rgb, pointColor.rgb, pct);
    color.a   = mix(color.a,   pointColor.a,   pct);
  }

  // Perlin noise 色彩调制（对齐 Android）
  color.rgb += noiseValue * 0.04 * color.rgb;

  // Premultiplied alpha 反转（对齐 Android）
  color.rgb /= max(color.a, 1e-5);

  // HSV 色彩增强 — colorPulse 驱动（对齐 Android）
  float colorPulse = clamp(0.68 * level + 0.32 * beat, 0.0, 1.0);
  vec3 hsv = rgb2hsv(color.rgb);
  // 暗区饱和度限制（对齐 Android dark-saturation limiter）
  float satBoost = colorPulse * 0.18 * u_saturateOffset;
  if (hsv.z < 0.3) satBoost *= hsv.z / 0.3;
  hsv.y = clamp(hsv.y + satBoost, 0.0, 1.0);
  hsv.z = clamp(hsv.z + colorPulse * 0.08 * u_lightOffset, 0.0, 1.0);
  color.rgb = hsv2rgb(hsv);

  // 垂直 vignette（对齐 Android）
  float vig = smoothstep(0.0, 0.35, vUv.y) * smoothstep(1.0, 0.65, vUv.y);
  color.rgb *= mix(0.7, 1.0, vig);

  // 透明度 + 轻度呼吸（对齐 Android）
  color.a = clamp(color.a, 0.0, 1.0);
  float alphaMod = clamp(1.0 - 0.18 * level - 0.12 * beat, 0.55, 1.0);
  color.a *= alphaMod;

  // 颗粒抖动（对齐 Android gradientNoise 5/255 消除色带）
  color.rgb += (10.0 / 255.0) * gradientNoise(gl_FragCoord.xy) - (5.0 / 255.0);

  gl_FragColor = vec4(clamp(color.rgb, 0.0, 1.0) * color.a, color.a);
}
