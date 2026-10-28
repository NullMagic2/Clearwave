// Portable tensor operators for the resident feature graph and diagnostics.
// Parameters carry tensor dimensions so channel and gate layouts remain explicit.
// Stored weight order is adapted by graph setup before dispatch.
const CONVOLUTION: u32 = 0u;
const NORMALIZE: u32 = 1u;
const ACTIVATE: u32 = 2u;
const GRU: u32 = 3u;
const CONCATENATE: u32 = 4u;
const PROJECT: u32 = 5u;
const NORMALIZATION_EPSILON: f32 = 0.001;
const OPERATOR_GROUP_SIZE: u32 = 64u;
const RESET_GATE: u32 = 0u;
const UPDATE_GATE: u32 = 1u;
const NEW_GATE: u32 = 2u;
const GRU_GATES: u32 = 3u;
const RECURRENT_BIAS: u32 = GRU_GATES;
const PREVIOUS_STATE: u32 = 2u * GRU_GATES;
struct Params { op:u32, count:u32, channels:u32, height:u32, width:u32, out_channels:u32, out_height:u32, kh:u32, kw:u32, pad_width:u32, mode:u32, spare:u32 }
@group(0) @binding(0) var<storage,read> weights:array<f32>;
@group(0) @binding(1) var<storage,read> input_data:array<f32>;
@group(0) @binding(2) var<storage,read> auxiliary:array<f32>;
@group(0) @binding(3) var<storage,read_write> output_data:array<f32>;
@group(0) @binding(4) var<uniform> p:Params;
fn sigmoid(x:f32)->f32 { return 1.0/(1.0+exp(-x)); }
@compute @workgroup_size(OPERATOR_GROUP_SIZE)
fn main(@builtin(global_invocation_id) gid:vec3<u32>) {
 let i=gid.x; if(i>=p.count){return;}
 if(p.op==CONVOLUTION){
  let x=i%p.width; let t=(i/p.width)%p.out_height; let c=i/(p.width*p.out_height);
  let kernel_count=p.out_channels*p.channels*p.kh*p.kw;
  var value=weights[kernel_count+c];
  for(var ci=0u;ci<p.channels;ci++){
   for(var kt=0u;kt<p.kh;kt++){
    for(var kx=0u;kx<p.kw;kx++){
     let sx=i32(x+kx)-i32(p.pad_width);
     if(sx>=0 && sx<i32(p.width)){
      let a=(ci*p.height+t+kt)*p.width+u32(sx);
      let b=((c*p.channels+ci)*p.kh+kt)*p.kw+kx;
      value+=input_data[a]*weights[b];
     }
    }
   }
  }
  if(p.mode==1u){value=max(value,0.0);} output_data[i]=value;
 }else if(p.op==NORMALIZE){
  let c=i/(p.height*p.width);let n=p.channels;
  output_data[i]=(input_data[i]-weights[2u*n+c])*inverseSqrt(weights[3u*n+c]+NORMALIZATION_EPSILON)*weights[c]+weights[n+c];
 }else if(p.op==ACTIVATE){
  let x=input_data[i];var y=x;
  if(p.mode==0u){y=max(x,0.0);}else if(p.mode==1u){y=sigmoid(x);}else{y=tanh(x);}
  output_data[i]=y;
 }else if(p.op==GRU){
  let n=p.count;
  let reset=RESET_GATE*n+i;
  let update=UPDATE_GATE*n+i;
  let candidate=NEW_GATE*n+i;
  let recurrent_bias=RECURRENT_BIAS*n;
  let r=sigmoid(input_data[reset]+weights[reset]+auxiliary[reset]+weights[recurrent_bias+reset]);
  let z=sigmoid(input_data[update]+weights[update]+auxiliary[update]+weights[recurrent_bias+update]);
  let new_hidden=tanh(input_data[candidate]+weights[candidate]+r*(auxiliary[candidate]+weights[recurrent_bias+candidate]));
  output_data[i]=z*weights[PREVIOUS_STATE*n+i]+(1.0-z)*new_hidden;
 }else if(p.op==CONCATENATE){
  // The first segment is the noise GRU hidden vector.
  // The remainder permutes stored channel planes into frequency/channel order.
  // CPU and GPU receive the same dimensions from the graph contract.
  if(i<p.height){output_data[i]=input_data[i];}
  else{let j=i-p.height;output_data[i]=auxiliary[(j%p.channels)*p.width+j/p.channels];}
 }else if(p.op==PROJECT){
  output_data[i]=sigmoid(input_data[i]+weights[i]);
 }
}
