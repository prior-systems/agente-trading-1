using Statistics, LinearAlgebra

# ── Hidden Markov Model for regime detection ──────────────────────────────────
# Models the market as switching between K latent regimes
# Each regime has its own (μ, σ) — Gaussian emissions
# Regime is a DISTRIBUTION, not a binary state
# Output: P(regime_k | observations) for each t

struct HMMParams
    K::Int                          # number of regimes
    π::Vector{Float64}              # initial state distribution
    A::Matrix{Float64}              # K×K transition matrix
    μ::Vector{Float64}              # regime means
    σ::Vector{Float64}              # regime standard deviations
    log_likelihood::Float64
end

struct RegimeState
    probabilities::Vector{Float64}  # P(regime_k) for each k, sums to 1
    most_likely::Int                # argmax of probabilities
    transition_risk::Float64        # entropy of distribution (0=certain, log(K)=uniform)
    # Labeled interpretation
    labels::Vector{Symbol}          # e.g. [:low_vol, :normal, :stress]
end

# ── Gaussian HMM via Baum-Welch ───────────────────────────────────────────────

function _emission_log_prob(x::Float64, μ::Float64, σ::Float64)::Float64
    -0.5 * log(2π) - log(σ) - 0.5 * ((x - μ) / σ)^2
end

# Forward algorithm: α[t,k] = P(o_1..o_t, s_t=k)
function _forward(obs::Vector{Float64}, p::HMMParams)
    n = length(obs)
    K = p.K
    α = zeros(n, K)
    log_α = fill(-Inf, n, K)

    # t=1
    for k in 1:K
        log_α[1, k] = log(p.π[k]) + _emission_log_prob(obs[1], p.μ[k], p.σ[k])
    end

    for t in 2:n
        for k in 1:K
            # log-sum-exp over previous states
            vals = [log_α[t-1, j] + log(p.A[j, k]) for j in 1:K]
            log_α[t, k] = _logsumexp(vals) + _emission_log_prob(obs[t], p.μ[k], p.σ[k])
        end
    end
    return log_α
end

# Backward algorithm: β[t,k] = P(o_{t+1}..o_n | s_t=k)
function _backward(obs::Vector{Float64}, p::HMMParams)
    n = length(obs)
    K = p.K
    log_β = fill(-Inf, n, K)
    log_β[n, :] .= 0.0

    for t in n-1:-1:1
        for j in 1:K
            vals = [log(p.A[j, k]) + _emission_log_prob(obs[t+1], p.μ[k], p.σ[k]) +
                    log_β[t+1, k] for k in 1:K]
            log_β[t, j] = _logsumexp(vals)
        end
    end
    return log_β
end

function _logsumexp(xs::Vector{Float64})::Float64
    m = maximum(xs)
    isinf(m) && return -Inf
    return m + log(sum(exp.(xs .- m)))
end

# Baum-Welch EM algorithm
function fit_hmm(
    obs::Vector{Float64},
    K::Int = 3;
    max_iter::Int = 200,
    tol::Float64 = 1e-6
)::HMMParams
    n = length(obs)

    # Initialize with k-means-like partition
    sorted_idx = sortperm(obs)
    chunk = n ÷ K
    μ = [mean(obs[sorted_idx[((k-1)*chunk+1):min(k*chunk, n)]]) for k in 1:K]
    σ = [std(obs) / K for _ in 1:K]
    π = fill(1.0/K, K)
    A = fill(1.0/K, K, K)

    ll_prev = -Inf

    for iter in 1:max_iter
        p = HMMParams(K, π, A, μ, σ, 0.0)
        log_α = _forward(obs, p)
        log_β = _backward(obs, p)

        # log P(o_{1..n})
        ll = _logsumexp(log_α[n, :])

        # Posterior: γ[t,k] = P(s_t=k | obs)
        log_γ = log_α .+ log_β
        for t in 1:n
            log_γ[t, :] .-= _logsumexp(log_γ[t, :])
        end
        γ = exp.(log_γ)

        # ξ[t,j,k] = P(s_t=j, s_{t+1}=k | obs) — needed for A update
        # Update transition matrix
        A_new = zeros(K, K)
        for j in 1:K, k in 1:K
            for t in 1:n-1
                A_new[j, k] += exp(log_α[t,j] + log(A[j,k]) +
                                   _emission_log_prob(obs[t+1], μ[k], σ[k]) +
                                   log_β[t+1,k] - ll)
            end
        end
        # Normalize rows
        for j in 1:K
            row_sum = sum(A_new[j, :])
            row_sum > 0 && (A_new[j, :] ./= row_sum)
        end

        # Update emissions
        μ_new = [sum(γ[:, k] .* obs) / max(sum(γ[:, k]), 1e-10) for k in 1:K]
        σ_new = [√(sum(γ[:, k] .* (obs .- μ_new[k]).^2) /
                    max(sum(γ[:, k]), 1e-10)) + 1e-8 for k in 1:K]

        π_new = γ[1, :]

        μ, σ, A, π = μ_new, σ_new, A_new, π_new
        abs(ll - ll_prev) < tol && break
        ll_prev = ll
    end

    return HMMParams(K, π, A, μ, σ, ll_prev)
end

# ── Regime detection ──────────────────────────────────────────────────────────

struct RegimeDetector
    hmm::HMMParams
    obs_history::Vector{Float64}  # rolling window of observations
    window::Int
end

RegimeDetector(returns::Vector{Float64}, K::Int=3, window::Int=63) =
    RegimeDetector(fit_hmm(returns, K), returns, window)

# Label regimes by their mean vol: low → normal → stress
function _label_regimes(hmm::HMMParams)::Vector{Symbol}
    labels = [:low_vol, :normal_vol, :stress_vol, :crisis_vol]
    order = sortperm(hmm.μ)  # sort by mean observation (vol)
    result = Vector{Symbol}(undef, hmm.K)
    for (rank, k) in enumerate(order)
        result[k] = rank ≤ length(labels) ? labels[rank] : Symbol("regime_$rank")
    end
    return result
end

function current_regime(det::RegimeDetector)::RegimeState
    hmm = det.hmm
    obs = det.obs_history[max(1, end-det.window+1):end]

    # Run forward algorithm on recent window
    log_α = _forward(obs, hmm)
    # Last time step posterior
    log_post = log_α[end, :]
    log_post .-= _logsumexp(log_post)
    probs = exp.(log_post)

    # Shannon entropy as transition_risk measure
    entropy = -sum(p * log(max(p, 1e-10)) for p in probs)
    max_entropy = log(hmm.K)

    labels = _label_regimes(hmm)
    RegimeState(probs, argmax(probs), entropy / max_entropy, labels)
end

function regime_probabilities(det::RegimeDetector)::Matrix{Float64}
    hmm = det.hmm
    obs = det.obs_history
    n = length(obs)
    log_α = _forward(obs, hmm)
    log_β = _backward(obs, hmm)
    log_γ = log_α .+ log_β
    for t in 1:n
        log_γ[t, :] .-= _logsumexp(log_γ[t, :])
    end
    return exp.(log_γ)   # n × K matrix
end
