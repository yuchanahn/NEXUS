# NEXUS 개발 작업 폴더

각 구성요소는 `projects/` 아래에서 독립적인 Git 저장소와 빌드 환경으로 관리합니다. 상위 저장소는 하위 프로젝트의 소스나 `.git`을 추적하지 않습니다. 현재 프로젝트는 Rust `projects/llm-provider`와 사용자 브릿지 포크 기반 `projects/nexus-web-harness`입니다.

프로젝트 분리와 실행 프로세스 분리는 별도로 관리합니다. 에이전트·도구 실행기 등 향후 프로젝트도 각각 자체 Git 저장소, 버전, 테스트, 배포 구성을 갖춥니다. 비어 있는 프로젝트를 미리 생성하지 않습니다.

`labs/`는 공급자 연구·호출 실험을 위한 별도 Git 저장소입니다. 32개 upstream 소스를 검토했고 production Rust 서버의 현재 등록은 실공급 경로 10개·모델 라우트 67개입니다(mock 포함 서버 ID 11개). 기존 공급자 경유 생성 성공 이력은 55개이며 Codex 기본 별칭을 제외하면 54개 경로입니다. 후속 NEXUS 경로는 실제 도구 호출·외부 결과 재개·Docker·Rust 클라이언트를 검증했습니다. 웹 Light/Medium/High는 같은 브라우저 모델의 추론 모드입니다. [최신 프로바이더 완성 보고서](projects/llm-provider/docs/provider-completion.md)와 [모델별 실험 기록](labs/reports/final-provider-report.md)에 성공·실패·지원 경계를 정리했습니다. 복제본과 인증 파일은 Git에 넣지 않습니다.
