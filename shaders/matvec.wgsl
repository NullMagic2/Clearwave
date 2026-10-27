struct Shape { rows: u32, cols: u32, unused0: u32, unused1: u32 }
@group(0) @binding(0) var<storage, read> matrix: array<f32>;
@group(0) @binding(1) var<storage, read> vector: array<f32>;
@group(0) @binding(2) var<storage, read_write> result: array<f32>;
@group(0) @binding(3) var<uniform> shape: Shape;
var<workgroup> partial: array<f32, 256>;

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let row = group.x;
    var sum = 0.0;
    for (var col = lane; col < shape.cols; col += 256u) {
        sum += matrix[row * shape.cols + col] * vector[col];
    }
    partial[lane] = sum;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width /= 2u) {
        if (lane < width) { partial[lane] += partial[lane + width]; }
        workgroupBarrier();
    }
    if (lane == 0u && row < shape.rows) { result[row] = partial[0]; }
}
