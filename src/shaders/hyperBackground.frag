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

// 色彩空间工具
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

// 颗粒抖动
float gradientNoise(vec2 uv) {
  return fract(52.9829189 * fract(dot(uv, vec2(0.06711056, 0.00583715))));
}

void main() {
  vec2 vUv = gl_FragCoord.xy / u_resolution;
  vec2 uv = vUv;

  float levelEase = clamp(u_musicLevel, 0.0, 1.0);
  float beatEase = clamp(u_beat, 0.0, 1.0);
  float motionEase = clamp(0.42 * levelEase + 0.82 * beatEase, 0.0, 1.0);
  float colorPulse = clamp(0.68 * levelEase + 0.32 * beatEase, 0.0, 1.0);
  float zoom = 1.0 + 0.024 * levelEase + 0.105 * beatEase;
  vec2 center = vec2(0.5);
  uv = (uv - center) / zoom + center;

  float beatWave = sin((vUv.y + u_time * 0.12) * 6.2832) *
    cos((vUv.x - u_time * 0.10) * 6.2832);
  float radialPulse = sin((distance(vUv, center) * 5.5 - u_time * 0.18) * 6.2832);
  float ribbonWave = sin(((vUv.x * 1.7 + vUv.y * 2.3) - u_time * 0.12) * 6.2832);
  uv += beatEase * 0.0110 * vec2(beatWave, -beatWave);
  uv += beatEase * 0.0085 * normalize(vUv - center + vec2(1e-4)) * radialPulse;
  uv += motionEase * 0.0062 * vec2(ribbonWave, -ribbonWave * 0.75);
  uv += motionEase * 0.0060 * vec2(sin(u_time * 1.9), cos(u_time * 1.6));

  vec3 points[5];
  if (u_darkMode > 0.5) {
    points[0] = vec3(0.52, 0.48, 0.90);
    points[1] = vec3(0.14, 0.32, 0.72);
    points[2] = vec3(0.92, 0.28, 0.76);
    points[3] = vec3(0.24, 0.88, 0.78);
    points[4] = vec3(0.86, 0.86, 0.82);
  } else {
    points[0] = vec3(0.52, 0.46, 0.92);
    points[1] = vec3(0.14, 0.32, 0.74);
    points[2] = vec3(0.92, 0.30, 0.76);
    points[3] = vec3(0.26, 0.88, 0.80);
    points[4] = vec3(0.84, 0.86, 0.84);
  }

  vec4 colors[5];
  colors[0] = u_color0;
  colors[1] = u_color1;
  colors[2] = u_color2;
  colors[3] = u_color3;
  colors[4] = u_color4;

  float pointOffset = 0.1 + 0.022 * levelEase + 0.108 * beatEase;
  float pointRadiusMulti = 1.0 + 0.045 * levelEase + 0.220 * beatEase;
  vec4 colorAccum = vec4(0.0);
  float weightSum = 0.0;

  for (int i = 0; i < 5; i++) {
    vec4 pointColor = colors[i];
    pointColor.rgb *= pointColor.a;
    float x = points[i].x;
    float y = points[i].y;
    float radius = points[i].z * pointRadiusMulti;

    x += sin(u_time + y) * pointOffset;
    y += cos(u_time + x) * pointOffset;

    float pushX = x - 0.5 + 1.0e-4;
    float pushY = y - 0.5 + 1.0e-4;
    float pushLength = sqrt(pushX * pushX + pushY * pushY);
    float pushScale = pushLength > 0.0 ? beatEase * 0.118 / pushLength : 0.0;
    vec2 point = vec2(x + pushX * pushScale, y + pushY * pushScale);

    vec2 delta = uv - point;
    float radiusSq = max(radius * radius, 1e-4);
    float weight = 1.0 / (1.0 + dot(delta, delta) / radiusSq * 9.3);
    weight *= weight;

    colorAccum += pointColor * weight;
    weightSum += weight;
  }

  vec4 color = colorAccum / max(weightSum, 1e-5);
  color.rgb /= max(color.a, 1e-5);

  vec3 hsv = rgb2hsv(color.rgb);
  float boostedSaturation = hsv.y * (1.16 + 0.06 * colorPulse) + 0.030 * colorPulse * u_saturateOffset;
  float darkSaturationLimit = mix(0.66, 0.82, smoothstep(0.34, 0.74, hsv.z));
  hsv.y = clamp(min(boostedSaturation, darkSaturationLimit), 0.0, 1.0);
  hsv.z = clamp((hsv.z - 0.5) * (1.26 + 0.06 * colorPulse) + 0.5 + 0.018 * colorPulse, 0.0, 1.0);
  color.rgb = hsv2rgb(hsv);
  color.rgb += 0.010 * colorPulse * u_lightOffset;
  color.rgb *= mix(0.68, 1.10, smoothstep(0.10, 0.92, vUv.y));
  color.rgb = clamp(color.rgb, 0.0, 1.0);

  color.a = clamp(color.a, 0.0, 1.0);

  float dither = (gradientNoise(gl_FragCoord.xy) - 0.5) * (5.0 / 255.0);
  color.rgb = clamp(color.rgb + dither, 0.0, 1.0);

  gl_FragColor = vec4(color.rgb * color.a, color.a);
}
