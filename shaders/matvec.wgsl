// One workgroup evaluates each output row of a stored execution matrix.
// Threads accumulate strided columns, then reduce their partial dot products.
// The graph adapts input-major projections once before supplying this matrix.
const MATRIX_GROUP_SIZE: u32 = 256u;
struct Shape { rows: u32, cols: u32, unused0: u32, unused1: u32 }
@group(0) @binding(0) var<storage, read> matrix: array<f32>;
@group(0) @binding(1) var<storage, read> vector: array<f32>;
@group(0) @binding(2) var<storage, read_write> result: array<f32>;
@group(0) @binding(3) var<uniform> shape: Shape;
var<workgroup> partial: array<f32, MATRIX_GROUP_SIZE>;

@compute @workgroup_size(MATRIX_GROUP_SIZE)
fn main(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let row = group.x;
    var sum = 0.0;
    for (var col = lane; col < shape.cols; col += MATRIX_GROUP_SIZE) {
        sum += matrix[row * shape.cols + col] * vector[col];
    }
    partial[lane] = sum;
    workgroupBarrier();
    for (var width = MATRIX_GROUP_SIZE / 2u; width > 0u; width /= 2u) {
        if (lane < width) { partial[lane] += partial[lane + width]; }
        workgroupBarrier();
    }
    if (lane == 0u && row < shape.rows) { result[row] = partial[0]; }
}
