struct Params { op:u32, count:u32, channels:u32, height:u32, width:u32, out_channels:u32, out_height:u32, kh:u32, kw:u32, pad_width:u32, mode:u32, spare:u32 }
@group(0) @binding(0) var<storage,read> weights:array<f32>;
@group(0) @binding(1) var<storage,read> input_data:array<f32>;
@group(0) @binding(2) var<storage,read> auxiliary:array<f32>;
@group(0) @binding(3) var<storage,read_write> output_data:array<f32>;
@group(0) @binding(4) var<uniform> p:Params;
fn sigmoid(x:f32)->f32 { return 1.0/(1.0+exp(-x)); }
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid:vec3<u32>) {
 let i=gid.x; if(i>=p.count){return;}
 if(p.op==0u){
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
 }else if(p.op==1u){
  let c=i/(p.height*p.width);let n=p.channels;
  output_data[i]=(input_data[i]-weights[2u*n+c])*inverseSqrt(weights[3u*n+c]+0.001)*weights[c]+weights[n+c];
 }else if(p.op==2u){
  let x=input_data[i];var y=x;
  if(p.mode==0u){y=max(x,0.0);}else if(p.mode==1u){y=sigmoid(x);}else{y=tanh(x);}
  output_data[i]=y;
 }else if(p.op==3u){
  let n=p.count;
  let r=sigmoid(input_data[i]+weights[i]+auxiliary[i]+weights[3u*n+i]);
  let z=sigmoid(input_data[n+i]+weights[n+i]+auxiliary[n+i]+weights[4u*n+i]);
  let candidate=tanh(input_data[2u*n+i]+weights[2u*n+i]+r*(auxiliary[2u*n+i]+weights[5u*n+i]));
  output_data[i]=z*weights[6u*n+i]+(1.0-z)*candidate;
 }else if(p.op==4u){
  // Noise GRU output first, followed by frequency-major convolution features.
  if(i<256u){output_data[i]=input_data[i];}
  else{let j=i-256u;output_data[i]=auxiliary[(j%32u)*320u+j/32u];}
 }else if(p.op==5u){
  output_data[i]=sigmoid(input_data[i]+weights[i]);
 }
}
