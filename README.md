# NEXUS 개발 작업 폴더

2026-10-12 최종 도구 검증: 등록된 67개 경로를 모두 시도했고 **42개에서 툴콜·외부 결과 재입력 왕복**을 확인했습니다. 네이티브·명시적 에뮬레이션·웹 MCP와 미가용·출력 실패를 구분한 [모델별 도구 지원표](projects/llm-provider/docs/tool-verification.md)가 현재 기준입니다.

각 구성요소는 `projects/` 아래에서 독립적인 Git 저장소와 빌드 환경으로 관리합니다. 상위 저장소는 하위 프로젝트의 소스나 `.git`을 추적하지 않습니다. 현재 프로젝트는 Rust `projects/llm-provider`, 사용자 브릿지 포크 기반 `projects/nexus-web-harness`, Rust `projects/nexus-agent`입니다.

2026-10-13: [영속 에이전트](projects/nexus-agent/README.md)를 구현했습니다. 고정 턴 수 없이 목표 검증까지 실행하며, 도구 journal·체크포인트·원문 보존 압축·자식 에이전트·DM/게시판·피어 outbox를 갖춥니다. 코어는 호스트, 에이전트 도구는 코어가 마운트되지 않은 Docker 컨테이너에서 실행합니다. 현재 에이전트 공급자는 웹 브릿지 `nexus-web/chatgpt-web/medium`이며 Astra는 사용하지 않습니다. [검증 보고서](projects/nexus-agent/reports/verification.md)에 실제 테스트와 실행 중인 협업 서비스 개발 목표를 기록합니다.

프로젝트 분리와 실행 프로세스 분리는 별도로 관리합니다. 에이전트·도구 실행기 등 향후 프로젝트도 각각 자체 Git 저장소, 버전, 테스트, 배포 구성을 갖춥니다. 비어 있는 프로젝트를 미리 생성하지 않습니다.

`labs/`는 공급자 연구·호출 실험을 위한 별도 Git 저장소입니다. 32개 upstream 소스를 검토했고 production Rust 서버의 현재 등록은 실공급 경로 10개·모델 라우트 67개입니다(mock 포함 서버 ID 11개). 기존 공급자 경유 생성 성공 이력은 55개이며 Codex 기본 별칭을 제외하면 54개 경로입니다. 후속 NEXUS 경로는 실제 도구 호출·외부 결과 재개·Docker·Rust 클라이언트를 검증했습니다. 웹 Light/Medium/High는 같은 브라우저 모델의 추론 모드입니다. [최신 프로바이더 완성 보고서](projects/llm-provider/docs/provider-completion.md)와 [모델별 실험 기록](labs/reports/final-provider-report.md)에 성공·실패·지원 경계를 정리했습니다. 복제본과 인증 파일은 Git에 넣지 않습니다.
