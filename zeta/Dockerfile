FROM julia:1.11-bookworm

WORKDIR /app

# Install ZMQ system library
RUN apt-get update && apt-get install -y libzmq3-dev && rm -rf /var/lib/apt/lists/*

# Cache dependency resolution before copying source
COPY Project.toml Manifest.toml* ./
RUN julia --project=. -e 'using Pkg; Pkg.instantiate(); Pkg.precompile()' 2>&1

COPY src/ ./src/

CMD ["julia", "--project=.", "src/server.jl"]
